use std::{
    ops::Deref,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use magic::{
    Cookie,
    cookie::{Flags, Load},
};
use tokio::time;

#[derive(Debug)]
pub(crate) struct MagicCookie<'a> {
    using:  &'a AtomicBool,
    cookie: &'a Cookie<Load>,
}

impl Drop for MagicCookie<'_> {
    fn drop(&mut self) {
        self.using.swap(false, Ordering::Relaxed);
    }
}

impl Deref for MagicCookie<'_> {
    type Target = Cookie<Load>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.cookie
    }
}

#[derive(Debug)]
pub(crate) struct MagicCookiePool {
    cookies: Vec<(AtomicBool, Cookie<Load>)>,
}

unsafe impl Send for MagicCookiePool {}
unsafe impl Sync for MagicCookiePool {}

impl MagicCookiePool {
    pub(crate) fn new(size: usize) -> Option<Self> {
        assert!(size > 0);

        let mut cookies = Vec::with_capacity(size);

        for _ in 0..size {
            let cookie = match Cookie::open(Flags::MIME_TYPE) {
                Ok(cookie) => cookie,
                Err(_) => return None,
            };

            let cookie = match cookie.load(&["/usr/share/file/magic.mgc"].try_into().unwrap()) {
                Ok(cookie) => cookie,
                Err(_) => return None,
            };

            cookies.push((AtomicBool::new(false), cookie));
        }

        Some(Self {
            cookies,
        })
    }
}

impl MagicCookiePool {
    pub(crate) async fn acquire_cookie(&self) -> MagicCookie<'_> {
        loop {
            for (using, cookie) in self.cookies.iter() {
                if using.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
                {
                    return MagicCookie {
                        using,
                        cookie,
                    };
                }
            }

            time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub(crate) fn acquire_cookie_sync(&self) -> MagicCookie<'_> {
        loop {
            for (using, cookie) in self.cookies.iter() {
                if using.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
                {
                    return MagicCookie {
                        using,
                        cookie,
                    };
                }
            }

            thread::sleep(Duration::from_millis(10));
        }
    }
}
