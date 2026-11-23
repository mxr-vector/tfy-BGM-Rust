//! AVFoundation 播放时钟与 GPUI 的 CoreVideo 视频帧。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use block2::RcBlock;
use core_foundation::{base::TCFType, string::CFString};
use core_video::pixel_buffer::{
    CVPixelBuffer, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSDictionary, NSNumber, NSRect, NSString, NSURL};

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

#[link(name = "CoreImage", kind = "framework")]
unsafe extern "C" {}

#[derive(Clone, Copy)]
#[repr(C)]
struct MediaTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

unsafe impl Encode for MediaTime {
    const ENCODING: Encoding = Encoding::Struct(
        "?",
        &[i64::ENCODING, i32::ENCODING, u32::ENCODING, i64::ENCODING],
    );
}

unsafe impl RefEncode for MediaTime {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

#[repr(C)]
struct PixelBufferOpaque {
    _opaque: [u8; 0],
}

unsafe impl RefEncode for PixelBufferOpaque {
    const ENCODING_REF: Encoding = Encoding::Struct("__CVBuffer", &[]);
}

impl MediaTime {
    const ZERO: Self = Self {
        value: 0,
        timescale: 1,
        flags: 1,
        epoch: 0,
    };

    fn seconds(self) -> Option<f64> {
        (self.timescale > 0 && self.flags & 1 != 0 && self.flags & 0x1c == 0)
            .then(|| self.value as f64 / f64::from(self.timescale))
    }
}

#[repr(u8)]
enum Composition {
    Preparing,
    Ready,
    Failed,
}

pub struct Video {
    player: Retained<AnyObject>,
    item: Retained<AnyObject>,
    output: Retained<AnyObject>,
    composition: Arc<AtomicU8>,
    blur: Arc<AtomicU32>,
    received_frame: bool,
    playing: bool,
    looping: bool,
    waiting_since: Option<Instant>,
}

impl Video {
    pub fn open(path: &Path, blur: u32) -> Result<Self, String> {
        let path_text = path.to_str().ok_or("视频路径不是有效文本")?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path_text));
        let item: Retained<AnyObject> =
            unsafe { msg_send![class!(AVPlayerItem), playerItemWithURL: &*url] };
        let player: Retained<AnyObject> =
            unsafe { msg_send![class!(AVPlayer), playerWithPlayerItem: &*item] };
        unsafe {
            let _: () = msg_send![&*player, setMuted: true];
        }

        let pixel_format =
            unsafe { CFString::wrap_under_get_rule(kCVPixelBufferPixelFormatTypeKey) };
        let io_surface =
            unsafe { CFString::wrap_under_get_rule(kCVPixelBufferIOSurfacePropertiesKey) };
        let format_value = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
        let properties = NSDictionary::<NSString, AnyObject>::new();
        let keys = [
            unsafe { &*pixel_format.as_concrete_TypeRef().cast::<NSString>() },
            unsafe { &*io_surface.as_concrete_TypeRef().cast::<NSString>() },
        ];
        let objects: [&AnyObject; 2] = [(&*format_value).as_ref(), (&*properties).as_ref()];
        let attrs = NSDictionary::from_slices(&keys, &objects);
        let output: Retained<AnyObject> = unsafe {
            msg_send![msg_send![class!(AVPlayerItemVideoOutput), alloc], initWithPixelBufferAttributes: &*attrs]
        };
        unsafe {
            let _: () = msg_send![&*item, addOutput: &*output];
        }

        let radius = Arc::new(AtomicU32::new(blur));
        let filter_radius = radius.clone();
        let filter = RcBlock::new(move |request: *mut AnyObject| {
            let Some(request) = (unsafe { request.as_ref() }) else {
                return;
            };
            let image: Retained<AnyObject> = unsafe { msg_send![request, sourceImage] };
            let sigma = f64::from(filter_radius.load(Ordering::Relaxed));
            let result = if sigma > 0.0 {
                unsafe {
                    let rect: NSRect = msg_send![&*image, extent];
                    let clamped: Retained<AnyObject> = msg_send![&*image, imageByClampingToExtent];
                    let blurred: Retained<AnyObject> =
                        msg_send![&*clamped, imageByApplyingGaussianBlurWithSigma: sigma];
                    msg_send![&*blurred, imageByCroppingToRect: rect]
                }
            } else {
                image
            };
            unsafe {
                let _: () = msg_send![request, finishWithImage: &*result, context: std::ptr::null::<AnyObject>()];
            }
        });
        let composition = Arc::new(AtomicU8::new(Composition::Preparing as u8));
        let status = composition.clone();
        let item_for_composition = item.clone();
        let finish = RcBlock::new(move |result: *mut AnyObject, _error: *mut AnyObject| {
            if let Some(result) = unsafe { result.as_ref() } {
                unsafe {
                    let _: () = msg_send![&*item_for_composition, setVideoComposition: result];
                }
                status.store(Composition::Ready as u8, Ordering::Release);
            } else {
                status.store(Composition::Failed as u8, Ordering::Release);
            }
        });
        unsafe {
            let asset: Retained<AnyObject> = msg_send![&*item, asset];
            let _: () = msg_send![class!(AVVideoComposition), videoCompositionWithAsset: &*asset, applyingCIFiltersWithHandler: &*filter, completionHandler: &*finish];
        }

        Ok(Self {
            player,
            item,
            output,
            composition,
            blur: radius,
            received_frame: false,
            playing: false,
            looping: false,
            waiting_since: None,
        })
    }

    pub fn poll(&mut self, active: bool, blur: u32) -> Result<Option<CVPixelBuffer>, String> {
        self.blur.store(blur, Ordering::Relaxed);
        if !active {
            self.set_playing(false);
            self.waiting_since = None;
            return Ok(None);
        }
        let state = self.composition.load(Ordering::Acquire);
        let item_status: isize = unsafe { msg_send![&*self.item, status] };
        if state == Composition::Failed as u8 || item_status == 2 {
            return Err("无法解码 MP4 视频".into());
        }
        self.set_playing(state == Composition::Ready as u8);
        if !self.received_frame
            && self
                .waiting_since
                .get_or_insert_with(Instant::now)
                .elapsed()
                >= Duration::from_secs(5)
        {
            return Err("无法读取 MP4 视频画面".into());
        }
        if !self.playing {
            return Ok(None);
        }

        let time: MediaTime = unsafe { msg_send![&*self.player, currentTime] };
        let duration: MediaTime = unsafe { msg_send![&*self.item, duration] };
        if time
            .seconds()
            .zip(duration.seconds())
            .is_some_and(|(time, duration)| duration > 0.0 && time >= duration)
        {
            if !self.looping {
                unsafe {
                    let _: () = msg_send![&*self.player, seekToTime: MediaTime::ZERO, toleranceBefore: MediaTime::ZERO, toleranceAfter: MediaTime::ZERO];
                    let _: () = msg_send![&*self.player, play];
                }
                self.looping = true;
            }
            return Ok(None);
        }
        self.looping = false;
        let updated: bool = unsafe { msg_send![&*self.output, hasNewPixelBufferForItemTime: time] };
        if !updated {
            return Ok(None);
        }
        let pixels: *mut PixelBufferOpaque = unsafe {
            msg_send![&*self.output, copyPixelBufferForItemTime: time, itemTimeForDisplay: std::ptr::null_mut::<MediaTime>()]
        };
        if pixels.is_null() {
            return Ok(None);
        }
        let pixels = unsafe { CVPixelBuffer::wrap_under_create_rule(pixels.cast()) };
        if pixels.get_pixel_format() != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
            || pixels.get_plane_count() != 2
        {
            return Err("视频像素格式无法绘制".into());
        }
        self.received_frame = true;
        self.waiting_since = None;
        Ok(Some(pixels))
    }

    fn set_playing(&mut self, playing: bool) {
        if self.playing != playing {
            unsafe {
                if playing {
                    let _: () = msg_send![&*self.player, play];
                } else {
                    let _: () = msg_send![&*self.player, pause];
                }
            }
            self.playing = playing;
        }
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        self.set_playing(false);
    }
}
