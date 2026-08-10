#[cfg(target_os = "windows")]
use crate::{AtlasKey, AtlasTile, PlatformAtlas, Point};
use crate::{DevicePixels, Pixels, Result, SharedString, Size, size};
use smallvec::SmallVec;

use image::{Delay, Frame};
#[cfg(target_os = "windows")]
use parking_lot::Mutex;
#[cfg(target_os = "windows")]
use std::sync::Arc;
use std::{
    borrow::Cow,
    fmt,
    hash::Hash,
    sync::atomic::{AtomicUsize, Ordering::SeqCst},
};

/// A source of assets for this app to use.
pub trait AssetSource: 'static + Send + Sync {
    /// Load the given asset from the source path.
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>>;

    /// List the assets at the given path.
    fn list(&self, path: &str) -> Result<Vec<SharedString>>;
}

impl AssetSource for () {
    fn load(&self, _path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(None)
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

/// A unique identifier for the image cache
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ImageId(pub usize);

impl ImageId {
    fn next() -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        Self(NEXT_ID.fetch_add(1, SeqCst))
    }
}

#[derive(PartialEq, Eq, Hash, Clone)]
#[expect(missing_docs)]
pub struct RenderImageParams {
    pub image_id: ImageId,
    pub frame_index: usize,
}

/// A cached and processed image, in BGRA format
pub struct RenderImage {
    /// The ID associated with this image
    pub id: ImageId,
    /// The scale factor of this image on render.
    pub(crate) scale_factor: f32,
    data: SmallVec<[Frame; 1]>,
}

impl PartialEq for RenderImage {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for RenderImage {}

impl RenderImage {
    /// Create a new image from the given data.
    pub fn new(data: impl Into<SmallVec<[Frame; 1]>>) -> Self {
        Self {
            id: ImageId::next(),
            scale_factor: 1.0,
            data: data.into(),
        }
    }

    /// Convert this image into a byte slice.
    pub fn as_bytes(&self, frame_index: usize) -> Option<&[u8]> {
        self.data
            .get(frame_index)
            .map(|frame| frame.buffer().as_raw().as_slice())
    }

    /// Get the size of this image, in pixels.
    pub fn size(&self, frame_index: usize) -> Size<DevicePixels> {
        self.data
            .get(frame_index)
            .map(|frame| {
                let (width, height) = frame.buffer().dimensions();
                size(width.into(), height.into())
            })
            .unwrap_or_default()
    }

    /// Get the size of this image, in pixels for display, adjusted for the scale factor.
    pub(crate) fn render_size(&self, frame_index: usize) -> Size<Pixels> {
        self.size(frame_index)
            .map(|v| (v.0 as f32 / self.scale_factor).into())
    }

    /// Get the delay of this frame from the previous
    pub fn delay(&self, frame_index: usize) -> Delay {
        self.data
            .get(frame_index)
            .map(|frame| frame.delay())
            .unwrap_or(Delay::from_numer_denom_ms(100, 1))
    }

    /// Get the number of frames for this image.
    pub fn frame_count(&self) -> usize {
        self.data.len()
    }
}

impl fmt::Debug for RenderImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageData")
            .field("id", &self.id)
            .field("size", &self.data.first().map(|f| f.buffer().dimensions()))
            .finish()
    }
}

/// A stable GPUI atlas image updated from callback-scoped D3D11 shared
/// textures. This is used by accelerated CEF off-screen rendering on Windows.
/// Pixel data never crosses the CPU; each update is copied into a GPUI-owned
/// atlas tile before CEF releases its source handle.
#[cfg(target_os = "windows")]
pub struct D3D11ExternalImage {
    atlas: Arc<dyn PlatformAtlas>,
    owner_thread: std::thread::ThreadId,
    state: Mutex<D3D11ExternalImageState>,
}

#[cfg(target_os = "windows")]
struct D3D11ExternalImageState {
    params: RenderImageParams,
    size: Size<DevicePixels>,
}

// GPUI's Windows atlas is backed by a mutex and D3D11's device/context are
// free-threaded COM interfaces. CEF currently invokes the sink on the same UI
// thread as GPUI, but its handler type requires a Send + Sync owner.
#[cfg(target_os = "windows")]
unsafe impl Send for D3D11ExternalImage {}
#[cfg(target_os = "windows")]
unsafe impl Sync for D3D11ExternalImage {}

#[cfg(target_os = "windows")]
impl D3D11ExternalImage {
    pub(crate) fn new(atlas: Arc<dyn PlatformAtlas>) -> Self {
        Self {
            atlas,
            owner_thread: std::thread::current().id(),
            state: Mutex::new(D3D11ExternalImageState {
                params: RenderImageParams {
                    image_id: ImageId::next(),
                    frame_index: 0,
                },
                size: Size::default(),
            }),
        }
    }

    /// Copy a CEF shared texture into this image. `shared_handle` remains owned
    /// by CEF and is valid only for the duration of this call.
    pub fn update_from_shared_texture(
        &self,
        shared_handle: usize,
        source_origin: Point<DevicePixels>,
        size: Size<DevicePixels>,
    ) -> Result<()> {
        if std::thread::current().id() != self.owner_thread {
            anyhow::bail!("D3D11 external image updated from the wrong thread");
        }
        if shared_handle == 0 || size.width.0 <= 0 || size.height.0 <= 0 {
            anyhow::bail!("invalid D3D11 shared texture");
        }

        let mut state = self.state.lock();
        if state.size != size {
            if state.size != Size::default() {
                self.atlas.remove(&AtlasKey::Image(state.params.clone()));
            }
            state.params.image_id = ImageId::next();
            state.size = size;
        }

        let tile = self.atlas.copy_d3d11_shared_texture(
            &AtlasKey::Image(state.params.clone()),
            shared_handle,
            source_origin,
            size,
        )?;
        if tile.is_none() {
            anyhow::bail!("active GPUI renderer cannot import D3D11 shared textures");
        }
        Ok(())
    }

    pub(crate) fn tile(&self) -> Result<Option<AtlasTile>> {
        if std::thread::current().id() != self.owner_thread {
            anyhow::bail!("D3D11 external image painted from the wrong thread");
        }
        let state = self.state.lock();
        if state.size == Size::default() {
            return Ok(None);
        }
        self.atlas
            .get_or_insert_with(&AtlasKey::Image(state.params.clone()), &mut || Ok(None))
    }

    /// Whether the image's atlas tile survived the latest device state.
    pub fn is_available(&self) -> bool {
        self.tile().ok().flatten().is_some()
    }

    /// Physical size of the most recently copied texture.
    pub fn size(&self) -> Size<DevicePixels> {
        self.state.lock().size
    }
}

#[cfg(target_os = "windows")]
impl Drop for D3D11ExternalImage {
    fn drop(&mut self) {
        if std::thread::current().id() != self.owner_thread {
            return;
        }
        let state = self.state.lock();
        if state.size != Size::default() {
            self.atlas.remove(&AtlasKey::Image(state.params.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::SmallVec;

    #[test]
    fn empty_render_image_does_not_panic() {
        let image = RenderImage::new(SmallVec::new());
        assert_eq!(image.frame_count(), 0);
        assert_eq!(image.size(0), Size::default());
        assert_eq!(image.as_bytes(0), None);
        assert_eq!(image.render_size(0), Size::default());
        assert_eq!(image.delay(0), Delay::from_numer_denom_ms(100, 1));
        let _ = format!("{image:?}");
    }
}
