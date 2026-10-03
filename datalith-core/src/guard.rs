use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
};

use uuid::Uuid;

use crate::Datalith;

#[derive(Debug, Default)]
pub(crate) struct FileLifecycle {
    pub(crate) opening:  HashMap<Uuid, usize>,
    pub(crate) deleting: HashSet<Uuid>,
}

#[derive(Debug)]
pub(crate) struct PutGuard {
    _datalith: Datalith,
    hash:      [u8; 32],
}

impl Drop for PutGuard {
    fn drop(&mut self) {
        self._datalith.0._uploading_files.lock().unwrap().remove(&self.hash);
        self._datalith.0._file_changed.notify_waiters();
    }
}

impl PutGuard {
    pub async fn new(datalith: Datalith, hash: [u8; 32]) -> Self {
        loop {
            let changed = datalith.0._file_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if datalith.0._uploading_files.lock().unwrap().insert(hash) {
                break;
            }
            changed.await;
        }
        Self {
            _datalith: datalith,
            hash,
        }
    }
}

#[derive(Debug)]
pub(crate) struct OpenGuard {
    _datalith: Datalith,
    id:        Uuid,
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        let mut lifecycle = self._datalith.0._file_lifecycle.lock().unwrap();
        let count = lifecycle.opening.get_mut(&self.id).unwrap();
        *count -= 1;
        if *count == 0 {
            lifecycle.opening.remove(&self.id);
        }
        drop(lifecycle);
        self._datalith.0._file_changed.notify_waiters();
    }
}

impl OpenGuard {
    pub fn try_new(datalith: Datalith, id: Uuid) -> Option<Self> {
        {
            let mut lifecycle = datalith.0._file_lifecycle.lock().unwrap();
            if lifecycle.deleting.contains(&id) {
                return None;
            }
            *lifecycle.opening.entry(id).or_default() += 1;
        }
        Some(Self {
            _datalith: datalith,
            id,
        })
    }

    pub async fn new(datalith: Datalith, id: impl Into<Uuid>) -> Self {
        let id = id.into();
        loop {
            let changed = datalith.0._file_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut lifecycle = datalith.0._file_lifecycle.lock().unwrap();
                if !lifecycle.deleting.contains(&id) {
                    *lifecycle.opening.entry(id).or_default() += 1;
                    break;
                }
            }
            changed.await;
        }
        Self {
            _datalith: datalith,
            id,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DeleteGuard {
    _datalith:     Datalith,
    pub(crate) id: Uuid,
}

impl Drop for DeleteGuard {
    fn drop(&mut self) {
        self._datalith.0._file_lifecycle.lock().unwrap().deleting.remove(&self.id);
        self._datalith.0._file_changed.notify_waiters();
    }
}

impl DeleteGuard {
    pub fn try_acquire_multiple(datalith: Datalith, ids: &HashSet<Uuid>) -> Option<Vec<Self>> {
        let mut lifecycle = datalith.0._file_lifecycle.lock().unwrap();
        if ids
            .iter()
            .any(|id| lifecycle.deleting.contains(id) || lifecycle.opening.contains_key(id))
        {
            return None;
        }
        let mut guards = Vec::with_capacity(ids.len());
        for id in ids {
            lifecycle.deleting.insert(*id);
            guards.push(Self {
                _datalith: datalith.clone(), id: *id
            });
        }
        Some(guards)
    }

    pub async fn new(datalith: Datalith, id: impl Into<Uuid>) -> Self {
        let id = id.into();
        loop {
            let changed = datalith.0._file_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if datalith.0._file_lifecycle.lock().unwrap().deleting.insert(id) {
                break;
            }
            changed.await;
        }
        Self {
            _datalith: datalith,
            id,
        }
    }

    #[cfg(feature = "image-convert")]
    pub async fn acquire_multiple(guards: &mut Vec<Self>, datalith: Datalith, ids: &HashSet<Uuid>) {
        loop {
            let changed = datalith.0._file_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if Self::acquire_multiple_immediately(guards, datalith.clone(), ids).await {
                break;
            }
            changed.await;
        }
    }

    #[cfg(feature = "image-convert")]
    pub async fn acquire_multiple_immediately(
        guards: &mut Vec<Self>,
        datalith: Datalith,
        ids: &HashSet<Uuid>,
    ) -> bool {
        let mut lifecycle = datalith.0._file_lifecycle.lock().unwrap();
        if ids.iter().any(|id| lifecycle.deleting.contains(id)) {
            return false;
        }
        for id in ids {
            lifecycle.deleting.insert(*id);
            guards.push(Self {
                _datalith: datalith.clone(), id: *id
            });
        }
        true
    }
}

#[derive(Debug)]
pub(crate) struct TemporaryFileGuard {
    moved:     bool,
    file_path: PathBuf,
}

impl Drop for TemporaryFileGuard {
    #[inline]
    fn drop(&mut self) {
        if !self.moved {
            let _ = fs::remove_file(self.file_path.as_path());
        }
    }
}

impl TemporaryFileGuard {
    #[inline]
    pub fn new(file_path: impl Into<PathBuf>) -> Self {
        let file_path = file_path.into();

        Self {
            moved: false,
            file_path,
        }
    }

    #[inline]
    pub fn set_moved(&mut self) {
        self.moved = true;
    }
}
