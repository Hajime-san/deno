// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;

use deno_core::GarbageCollected;
use deno_core::StructuredCloneHostObject;
use deno_core::WebIDL;
use deno_core::op2;
use deno_core::v8;
use deno_core::webidl;
use deno_core::webidl::ContextFn;
use deno_core::webidl::IntOptions;
use deno_core::webidl::WebIdlConverter;
use deno_core::webidl::WebIdlError;
use deno_core::webidl::WebIdlErrorKind;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum ImageDataError {
  #[class(inherit)]
  #[error(transparent)]
  WebIDL(#[from] WebIdlError),
  #[class("DOMExceptionInvalidStateError")]
  #[error("Failed to construct 'ImageData': the input data has zero elements")]
  ZeroElements,
  #[class("DOMExceptionInvalidStateError")]
  #[error(
    "Failed to construct 'ImageData': the input data length is not a multiple of 4, received {0}"
  )]
  NotMultipleOfFour(usize),
  #[class("DOMExceptionIndexSizeError")]
  #[error(
    "Failed to construct 'ImageData': the source width is zero or not a number"
  )]
  ZeroWidth,
  #[class("DOMExceptionIndexSizeError")]
  #[error(
    "Failed to construct 'ImageData': the source height is zero or not a number"
  )]
  ZeroHeight,
  #[class("DOMExceptionIndexSizeError")]
  #[error(
    "Failed to construct 'ImageData': the input data length is not a multiple of (4 * width)"
  )]
  NotMultipleOfRow,
  #[class("DOMExceptionIndexSizeError")]
  #[error(
    "Failed to construct 'ImageData': the input data length is not equal to (4 * width * height)"
  )]
  WrongLength,
  #[class("DOMExceptionInvalidStateError")]
  #[error(
    "Failed to construct 'ImageData': Uint8ClampedArray must use rgba-unorm8 pixelFormat."
  )]
  Uint8ClampedNeedsUnorm,
  #[class("DOMExceptionInvalidStateError")]
  #[error(
    "Failed to construct 'ImageData': Float16Array must use rgba-float16 pixelFormat."
  )]
  Float16NeedsFloat16,
  #[class(generic)]
  #[error("Failed to allocate ImageData backing store")]
  AllocationFailed,
}

#[derive(WebIDL, Debug, Clone, Copy, PartialEq, Eq)]
#[webidl(enum)]
pub enum PredefinedColorSpace {
  #[webidl(rename = "srgb")]
  Srgb,
  #[webidl(rename = "display-p3")]
  DisplayP3,
}

impl PredefinedColorSpace {
  fn name(self) -> &'static str {
    match self {
      Self::Srgb => "srgb",
      Self::DisplayP3 => "display-p3",
    }
  }
}

#[derive(WebIDL, Debug, Clone, Copy, PartialEq, Eq)]
#[webidl(enum)]
pub enum ImageDataPixelFormat {
  #[webidl(rename = "rgba-unorm8")]
  RgbaUnorm8,
  #[webidl(rename = "rgba-float16")]
  RgbaFloat16,
}

impl ImageDataPixelFormat {
  fn name(self) -> &'static str {
    match self {
      Self::RgbaUnorm8 => "rgba-unorm8",
      Self::RgbaFloat16 => "rgba-float16",
    }
  }
}

#[derive(WebIDL, Debug)]
#[webidl(dictionary)]
pub struct ImageDataSettings {
  #[webidl(default = None)]
  color_space: Option<PredefinedColorSpace>,
  #[webidl(default = ImageDataPixelFormat::RgbaUnorm8)]
  pixel_format: ImageDataPixelFormat,
}

#[webidl(serializable)]
pub struct ImageData {
  width: u32,
  height: u32,
  pixel_format: ImageDataPixelFormat,
  color_space: PredefinedColorSpace,
  data: v8::TracedReference<v8::Object>,
}

// ImageData begins with a variable settings sequence. Each setting is encoded
// as a uint32(varint) subtag followed by its uint32(varint) value. End has no
// value and terminates the sequence. The fixed width, height, and V8-serialized
// typed array follow it. Missing settings use the Web API defaults, allowing
// new optional settings to be appended without changing the fixed payload.
// Subtag values are permanent and must remain reserved after retirement.
#[derive(Clone, Copy)]
#[repr(u32)]
enum ImageDataSerializationTag {
  // No value; terminates the settings sequence.
  End = 0,
  // Followed by SerializedPredefinedColorSpace.
  PredefinedColorSpace = 1,
  // Followed by SerializedImageDataPixelFormat.
  PixelFormat = 2,
  // Retired subtags must remain reserved and must never be reused.
}

impl ImageDataSerializationTag {
  fn from_u32(value: u32) -> Option<Self> {
    match value {
      value if value == Self::End as u32 => Some(Self::End),
      value if value == Self::PredefinedColorSpace as u32 => {
        Some(Self::PredefinedColorSpace)
      }
      value if value == Self::PixelFormat as u32 => Some(Self::PixelFormat),
      _ => None,
    }
  }
}

// Stable wire values, deliberately separate from the WebIDL enum declaration.
#[derive(Clone, Copy)]
#[repr(u32)]
enum SerializedPredefinedColorSpace {
  Srgb = 0,
  DisplayP3 = 1,
}

impl SerializedPredefinedColorSpace {
  fn from_color_space(value: PredefinedColorSpace) -> Self {
    match value {
      PredefinedColorSpace::Srgb => Self::Srgb,
      PredefinedColorSpace::DisplayP3 => Self::DisplayP3,
    }
  }

  fn into_color_space(value: u32) -> Option<PredefinedColorSpace> {
    match value {
      value if value == Self::Srgb as u32 => Some(PredefinedColorSpace::Srgb),
      value if value == Self::DisplayP3 as u32 => {
        Some(PredefinedColorSpace::DisplayP3)
      }
      _ => None,
    }
  }
}

// Stable wire values, deliberately separate from the WebIDL enum declaration.
#[derive(Clone, Copy)]
#[repr(u32)]
enum SerializedImageDataPixelFormat {
  RgbaUnorm8 = 0,
  RgbaFloat16 = 1,
}

impl SerializedImageDataPixelFormat {
  fn from_pixel_format(value: ImageDataPixelFormat) -> Self {
    match value {
      ImageDataPixelFormat::RgbaUnorm8 => Self::RgbaUnorm8,
      ImageDataPixelFormat::RgbaFloat16 => Self::RgbaFloat16,
    }
  }

  fn into_pixel_format(value: u32) -> Option<ImageDataPixelFormat> {
    match value {
      value if value == Self::RgbaUnorm8 as u32 => {
        Some(ImageDataPixelFormat::RgbaUnorm8)
      }
      value if value == Self::RgbaFloat16 as u32 => {
        Some(ImageDataPixelFormat::RgbaFloat16)
      }
      _ => None,
    }
  }
}

impl StructuredCloneHostObject for ImageData {
  fn write_structured_clone_payload<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    context: v8::Local<'s, v8::Context>,
    serializer: &dyn v8::ValueSerializerHelper,
  ) -> Option<bool> {
    serializer
      .write_uint32(ImageDataSerializationTag::PredefinedColorSpace as u32);
    serializer.write_uint32(SerializedPredefinedColorSpace::from_color_space(
      self.color_space,
    ) as u32);
    serializer.write_uint32(ImageDataSerializationTag::PixelFormat as u32);
    serializer.write_uint32(SerializedImageDataPixelFormat::from_pixel_format(
      self.pixel_format,
    ) as u32);
    serializer.write_uint32(ImageDataSerializationTag::End as u32);
    serializer.write_uint32(self.width);
    serializer.write_uint32(self.height);
    serializer.write_value(context, self.data.get(scope)?.into())
  }

  fn read_structured_clone_payload<'s, 'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    context: v8::Local<'s, v8::Context>,
    deserializer: &dyn v8::ValueDeserializerHelper,
    _wire_format_version: u32,
  ) -> Option<Self> {
    let mut pixel_format = ImageDataPixelFormat::RgbaUnorm8;
    let mut color_space = PredefinedColorSpace::Srgb;
    loop {
      let mut tag = 0;
      if !deserializer.read_uint32(&mut tag) {
        return None;
      }
      match ImageDataSerializationTag::from_u32(tag)? {
        ImageDataSerializationTag::End => break,
        ImageDataSerializationTag::PredefinedColorSpace => {
          let mut value = 0;
          if !deserializer.read_uint32(&mut value) {
            return None;
          }
          color_space =
            SerializedPredefinedColorSpace::into_color_space(value)?;
        }
        ImageDataSerializationTag::PixelFormat => {
          let mut value = 0;
          if !deserializer.read_uint32(&mut value) {
            return None;
          }
          pixel_format =
            SerializedImageDataPixelFormat::into_pixel_format(value)?;
        }
      }
    }

    let mut width = 0;
    let mut height = 0;
    if !deserializer.read_uint32(&mut width)
      || !deserializer.read_uint32(&mut height)
    {
      return None;
    }
    let data = deserializer
      .read_value(context)?
      .try_cast::<v8::Object>()
      .ok()?;
    let valid_data_type = match pixel_format {
      ImageDataPixelFormat::RgbaUnorm8 => data.is_uint8_clamped_array(),
      ImageDataPixelFormat::RgbaFloat16 => data.is_float16_array(),
    };
    let expected_length = (width as usize)
      .checked_mul(height as usize)?
      .checked_mul(4)?;
    let typed_array = v8::Local::<v8::TypedArray>::try_from(data).ok()?;
    if !valid_data_type || typed_array.length() != expected_length {
      return None;
    }

    Some(ImageData {
      width,
      height,
      pixel_format,
      color_space,
      data: v8::TracedReference::new(scope, data),
    })
  }
}

// SAFETY: we're sure `ImageData` can be GCed.
unsafe impl GarbageCollected for ImageData {
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    visitor.trace(&self.data);
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    <Self as deno_core::WebIdlInterface>::INTERFACE_NAME
  }
}

#[inline]
fn convert_unsigned_long<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  value: v8::Local<'a, v8::Value>,
  context: &'static str,
) -> Result<u32, WebIdlError> {
  u32::convert(
    scope,
    value,
    Cow::Borrowed("Failed to construct 'ImageData'"),
    ContextFn::new_borrowed(&|| Cow::Borrowed(context)),
    &IntOptions::default(),
  )
}

#[inline]
fn convert_settings<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  value: v8::Local<'a, v8::Value>,
  context: &'static str,
) -> Result<ImageDataSettings, WebIdlError> {
  ImageDataSettings::convert(
    scope,
    value,
    Cow::Borrowed("Failed to construct 'ImageData'"),
    ContextFn::new_borrowed(&|| Cow::Borrowed(context)),
    &Default::default(),
  )
}

#[inline]
fn alloc_typed_array<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  pixel_format: ImageDataPixelFormat,
  width: u32,
  height: u32,
) -> Result<v8::Local<'a, v8::Object>, ImageDataError> {
  let pixel_count = (width as usize)
    .checked_mul(height as usize)
    .and_then(|v| v.checked_mul(4))
    .ok_or(ImageDataError::AllocationFailed)?;
  match pixel_format {
    ImageDataPixelFormat::RgbaUnorm8 => {
      let buffer = v8::ArrayBuffer::new(scope, pixel_count);
      v8::Uint8ClampedArray::new(scope, buffer, 0, pixel_count)
        .map(Into::into)
        .ok_or(ImageDataError::AllocationFailed)
    }
    ImageDataPixelFormat::RgbaFloat16 => {
      let byte_length = pixel_count
        .checked_mul(2)
        .ok_or(ImageDataError::AllocationFailed)?;
      let buffer = v8::ArrayBuffer::new(scope, byte_length);
      let arr = v8::Float16Array::new(scope, buffer, 0, pixel_count)
        .ok_or(ImageDataError::AllocationFailed)?;
      // SAFETY: a Float16Array is a v8::Object (transmute is the existing
      // workaround until rusty_v8 implements `Into`).
      Ok(unsafe {
        std::mem::transmute::<v8::Local<v8::Float16Array>, v8::Local<v8::Object>>(
          arr,
        )
      })
    }
  }
}

#[op2]
impl ImageData {
  #[constructor]
  #[reentrant]
  #[required(2)]
  #[cppgc]
  fn constructor<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    #[varargs] args: Option<&v8::FunctionCallbackArguments<'a>>,
  ) -> Result<ImageData, ImageDataError> {
    // `#[required(2)]` ensures `args` has at least 2 entries.
    let args = args.expect("constructor requires arguments");
    let arg_count = args.length();
    let arg0 = args.get(0);
    let arg1 = args.get(1);
    let arg2 = if arg_count >= 3 {
      Some(args.get(2))
    } else {
      None
    };
    let arg3 = if arg_count >= 4 {
      Some(args.get(3))
    } else {
      None
    };

    let arg0_typed_array = v8::Local::<v8::TypedArray>::try_from(arg0).ok();
    let arg0_is_uint8_clamped = if arg0.is_object() {
      arg0.cast::<v8::Object>().is_uint8_clamped_array()
    } else {
      false
    };
    let arg0_is_float16 = if arg0.is_object() {
      arg0.cast::<v8::Object>().is_float16_array()
    } else {
      false
    };

    if arg_count > 3 || arg0_is_uint8_clamped || arg0_is_float16 {
      // Overload: new ImageData(data, sw [, sh [, settings ] ])
      let data = arg0_typed_array.ok_or_else(|| {
        WebIdlError::new(
          Cow::Borrowed("Failed to construct 'ImageData'"),
          ContextFn::new_borrowed(&|| Cow::Borrowed("Argument 1")),
          WebIdlErrorKind::ConvertToConverterType("ArrayBufferView"),
        )
      })?;
      let source_width = convert_unsigned_long(scope, arg1, "Argument 2")?;
      let source_height = match arg2 {
        Some(v) if !v.is_undefined() => {
          Some(convert_unsigned_long(scope, v, "Argument 3")?)
        }
        _ => None,
      };
      let settings_value = arg3.unwrap_or_else(|| v8::undefined(scope).into());
      let settings = convert_settings(scope, settings_value, "Argument 4")?;

      // Match `TypedArrayPrototypeGetLength` semantics: element count, not
      // byte count.
      let data_length = data.length();

      if data_length == 0 {
        return Err(ImageDataError::ZeroElements);
      }
      if data_length % 4 != 0 {
        return Err(ImageDataError::NotMultipleOfFour(data_length));
      }
      if source_width == 0 {
        return Err(ImageDataError::ZeroWidth);
      }
      if let Some(h) = source_height
        && h == 0
      {
        return Err(ImageDataError::ZeroHeight);
      }
      let pixel_count = data_length / 4;
      if pixel_count % source_width as usize != 0 {
        return Err(ImageDataError::NotMultipleOfRow);
      }
      let derived_height = (pixel_count / source_width as usize) as u32;
      if let Some(h) = source_height
        && h != derived_height
      {
        return Err(ImageDataError::WrongLength);
      }

      if arg0_is_uint8_clamped
        && !matches!(settings.pixel_format, ImageDataPixelFormat::RgbaUnorm8)
      {
        return Err(ImageDataError::Uint8ClampedNeedsUnorm);
      }
      if arg0_is_float16
        && !matches!(settings.pixel_format, ImageDataPixelFormat::RgbaFloat16)
      {
        return Err(ImageDataError::Float16NeedsFloat16);
      }

      let color_space =
        settings.color_space.unwrap_or(PredefinedColorSpace::Srgb);
      let height = source_height.unwrap_or(derived_height);
      let data_obj: v8::Local<v8::Object> = data.into();

      Ok(ImageData {
        width: source_width,
        height,
        pixel_format: settings.pixel_format,
        color_space,
        data: v8::TracedReference::new(scope, data_obj),
      })
    } else {
      // Overload: new ImageData(sw, sh [, settings])
      let source_width = convert_unsigned_long(scope, arg0, "Argument 1")?;
      let source_height = convert_unsigned_long(scope, arg1, "Argument 2")?;
      let settings_value = arg2.unwrap_or_else(|| v8::undefined(scope).into());
      let settings = convert_settings(scope, settings_value, "Argument 3")?;

      if source_width == 0 {
        return Err(ImageDataError::ZeroWidth);
      }
      if source_height == 0 {
        return Err(ImageDataError::ZeroHeight);
      }

      let data_obj = alloc_typed_array(
        scope,
        settings.pixel_format,
        source_width,
        source_height,
      )?;
      let color_space =
        settings.color_space.unwrap_or(PredefinedColorSpace::Srgb);

      Ok(ImageData {
        width: source_width,
        height: source_height,
        pixel_format: settings.pixel_format,
        color_space,
        data: v8::TracedReference::new(scope, data_obj),
      })
    }
  }

  #[fast]
  #[getter]
  fn width(&self) -> u32 {
    self.width
  }

  #[fast]
  #[getter]
  fn height(&self) -> u32 {
    self.height
  }

  #[getter]
  fn data<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> v8::Local<'a, v8::Object> {
    self.data.get(scope).unwrap()
  }

  #[getter]
  #[string]
  fn pixel_format(&self) -> &'static str {
    self.pixel_format.name()
  }

  #[getter]
  #[string]
  fn color_space(&self) -> &'static str {
    self.color_space.name()
  }
}
