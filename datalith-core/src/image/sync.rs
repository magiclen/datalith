use std::{ops::Deref, sync::Arc};

use image_convert::ImageResource;

#[derive(Debug, Clone)]
pub(crate) struct ReadOnlyImageResource {
    inner: Arc<ImageResource>,
}

unsafe impl Send for ReadOnlyImageResource {}
unsafe impl Sync for ReadOnlyImageResource {}

impl From<ImageResource> for ReadOnlyImageResource {
    #[inline]
    fn from(value: ImageResource) -> Self {
        Self {
            #[allow(clippy::arc_with_non_send_sync)]
            inner:                                          Arc::new(value),
        }
    }
}

impl Deref for ReadOnlyImageResource {
    type Target = ImageResource;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref()
    }
}
