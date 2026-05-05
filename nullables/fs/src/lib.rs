#![no_std]
extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
    rc::Rc,
    collections::{BTreeMap, BTreeSet},
};
use core::{
    cell::RefCell,
    ops::{Deref, DerefMut},
};
use rsnano_output_tracker::{OutputListener, OutputTracker};
use rsnano_sha::blake2b::Blake2bHasher;
use kernel_common::storage::bpm::BLOCK_SIZE;

// Mock PathBuf for no_std compatibility
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathBuf(String);

impl PathBuf {
    pub fn from(s: impl AsRef<str>) -> Self {
        Self(s.as_ref().to_string())
    }
}

impl<T: AsRef<str>> From<T> for PathBuf {
    fn from(s: T) -> Self {
        Self(s.as_ref().to_string())
    }
}

impl AsRef<Path> for PathBuf {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

// Mock Path for no_std compatibility
pub struct Path(str);

impl Path {
    pub fn new(s: &str) -> &Self {
        unsafe { &*(s as *const str as *const Path) }
    }
}

impl AsRef<str> for Path {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// Mock Error for no_std
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Error {
    kind: ErrorKind,
    msg: String,
}

impl Error {
    pub fn new(kind: ErrorKind, msg: impl Into<String>) -> Self {
        Self { kind, msg: msg.into() }
    }
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ErrorKind {
    NotFound,
    PermissionDenied,
    Other,
}

pub type Result<T> = core::result::Result<T, Error>;

// Mock Permissions
#[derive(Clone, Copy)]
pub struct Permissions;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsEvent {
    pub event_type: EventType,
    pub path: PathBuf,
    pub contents: String,
}

impl FsEvent {
    pub fn create_dir_all(path: impl Into<PathBuf>) -> Self {
        Self {
            event_type: EventType::CreateDirAll,
            path: path.into(),
            contents: String::new(),
        }
    }

    pub fn write(path: impl Into<PathBuf>, contents: impl AsRef<[u8]>) -> Self {
        Self {
            event_type: EventType::Write,
            path: path.into(),
            contents: String::from_utf8_lossy(contents.as_ref()).to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventType {
    CreateDirAll,
    Write,
}

pub struct NullableFilesystem {
    fs: RefCell<FsStrategy>,
    listener: OutputListener<FsEvent>,
}

#[allow(dead_code)]
enum FsStrategy {
    Real(RealFilesystem),
    Nulled(FilesystemStub),
}

impl Deref for FsStrategy {
    type Target = dyn Filesystem;

    fn deref(&self) -> &Self::Target {
        match self {
            FsStrategy::Real(i) => i,
            FsStrategy::Nulled(i) => i,
        }
    }
}

impl DerefMut for FsStrategy {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            FsStrategy::Real(i) => i,
            FsStrategy::Nulled(i) => i,
        }
    }
}

impl NullableFilesystem {
    pub fn new() -> Self {
        Self {
            fs: RefCell::new(FsStrategy::Real(RealFilesystem {})),
            listener: OutputListener::new(),
        }
    }

    #[allow(dead_code)]
    pub fn new_null() -> Self {
        Self {
            fs: RefCell::new(FsStrategy::Nulled(FilesystemStub::default())),
            listener: OutputListener::new(),
        }
    }

    #[allow(dead_code)]
    pub fn null_builder() -> NullableFilesystemBuilder {
        NullableFilesystemBuilder {
            stub: FilesystemStub::default(),
        }
    }

    pub fn exists(&self, f: impl AsRef<Path>) -> bool {
        self.fs.borrow().exists(f.as_ref())
    }

    pub fn read_to_string(&self, f: impl AsRef<Path>) -> Result<String> {
        self.fs.borrow_mut().read_to_string(f.as_ref())
    }

    pub fn create_dir_all(&self, f: impl AsRef<Path>) -> Result<()> {
        let path = f.as_ref();
        self.listener.emit(FsEvent::create_dir_all(PathBuf::from(path.as_ref())));
        self.fs.borrow_mut().create_dir_all(path)
    }

    pub fn write(&self, path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
        let path = path.as_ref();
        let contents = contents.as_ref();
        self.listener.emit(FsEvent::write(PathBuf::from(path.as_ref()), contents));
        self.fs.borrow_mut().write(path, contents)
    }

    pub fn set_permissions(
        &self,
        path: impl AsRef<Path>,
        permissions: Permissions,
    ) -> Result<()> {
        self.fs.borrow().set_permissions(path.as_ref(), permissions)
    }

    #[allow(dead_code)]
    pub fn track(&self) -> Rc<OutputTracker<FsEvent>> {
        self.listener.track()
    }
}

impl Default for NullableFilesystem {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
pub struct NullableFilesystemBuilder {
    stub: FilesystemStub,
}

#[allow(dead_code)]
impl NullableFilesystemBuilder {
    pub fn path_exists(mut self, path: impl Into<PathBuf>) -> Self {
        self.stub.exists.insert(path.into());
        self
    }

    pub fn read_to_string(mut self, path: impl Into<PathBuf>, contents: String) -> Self {
        self.stub.read_to_string.insert(path.into(), Ok(contents));
        self
    }

    pub fn read_to_string_fails(mut self, path: impl Into<PathBuf>, error: Error) -> Self {
        self.stub.read_to_string.insert(path.into(), Err(error));
        self
    }

    pub fn create_dir_all_fails(mut self, path: impl Into<PathBuf>, error: Error) -> Self {
        self.stub.create_dir_all_errors.insert(path.into(), error);
        self
    }

    pub fn write_fails(mut self, path: impl Into<PathBuf>, error: Error) -> Self {
        self.stub.write_errors.insert(path.into(), error);
        self
    }

    pub fn finish(self) -> NullableFilesystem {
        NullableFilesystem {
            fs: RefCell::new(FsStrategy::Nulled(self.stub)),
            listener: OutputListener::new(),
        }
    }
}

trait Filesystem {
    fn exists(&self, path: &Path) -> bool;
    fn read_to_string(&mut self, f: &Path) -> Result<String>;
    fn create_dir_all(&mut self, path: &Path) -> Result<()>;
    fn write(&mut self, path: &Path, contents: &[u8]) -> Result<()>;
    fn set_permissions(&self, path: &Path, permissions: Permissions) -> Result<()>;
}

struct RealFilesystem {}

impl RealFilesystem {
    pub const TABLE_ID: u8 = 0;

    fn hash_path(path: &Path) -> [u8; 65] {
        let mut hasher = Blake2bHasher::new(32);
        hasher.update(path.as_ref().as_bytes());
        let hash = hasher.finalize();
        let mut key = [0u8; 65];
        key[0] = Self::TABLE_ID;
        key[1..33].copy_from_slice(&hash);
        key
    }
}

impl Filesystem for RealFilesystem {
    fn exists(&self, path: &Path) -> bool {
        let key = Self::hash_path(path);
        let mut guard = kernel_common::storage::DB.lock();
        if let Some(btree) = guard.as_mut() {
            btree.search(&key).is_some()
        } else {
            false
        }
    }

    fn read_to_string(&mut self, f: &Path) -> Result<String> {
        let key = Self::hash_path(f);
        let mut guard = kernel_common::storage::DB.lock();
        let btree = guard.as_mut().ok_or(Error::new(ErrorKind::Other, "DB not initialized"))?;
        
        let val_bytes = btree.search(&key).ok_or(Error::new(ErrorKind::NotFound, "File not found"))?;
        let start_page = u32::from_le_bytes(val_bytes[0..4].try_into().unwrap());
        
        let mut data = Vec::new();
        let mut current_page = start_page;
        let mut is_first = true;
        let mut expected_len = 0;
        
        loop {
            if current_page == 0 { break; }
            let page_data = btree.read_page(current_page);
            let next_page = u32::from_le_bytes(page_data[0..4].try_into().unwrap());
            
            if is_first {
                expected_len = u64::from_le_bytes(page_data[4..12].try_into().unwrap()) as usize;
                let chunk_len = core::cmp::min(expected_len, BLOCK_SIZE - 12);
                data.extend_from_slice(&page_data[12..12 + chunk_len]);
                is_first = false;
            } else {
                let remaining = expected_len - data.len();
                let chunk_len = core::cmp::min(remaining, BLOCK_SIZE - 4);
                data.extend_from_slice(&page_data[4..4 + chunk_len]);
            }
            current_page = next_page;
        }
        
        String::from_utf8(data).map_err(|_| Error::new(ErrorKind::Other, "Invalid UTF-8"))
    }

    fn create_dir_all(&mut self, _path: &Path) -> Result<()> {
        // Virtual filesystem has no actual folders
        Ok(())
    }

    fn write(&mut self, path: &Path, contents: &[u8]) -> Result<()> {
        let key = Self::hash_path(path);
        let mut guard = kernel_common::storage::DB.lock();
        let btree = guard.as_mut().ok_or(Error::new(ErrorKind::Other, "DB not initialized"))?;
        
        let total_len = contents.len() as u64;
        let mut written = 0;
        let mut current_page = btree.allocate_page();
        let first_page = current_page;
        let mut is_first = true;
        
        while written < contents.len() || is_first {
            let mut page_data = [0u8; BLOCK_SIZE];
            
            let chunk_size = if is_first { BLOCK_SIZE - 12 } else { BLOCK_SIZE - 4 };
            let remaining = contents.len() - written;
            let to_write = core::cmp::min(chunk_size, remaining);
            
            let next_page = if remaining > to_write { btree.allocate_page() } else { 0 };
            
            page_data[0..4].copy_from_slice(&next_page.to_le_bytes());
            
            if is_first {
                page_data[4..12].copy_from_slice(&total_len.to_le_bytes());
                page_data[12..12 + to_write].copy_from_slice(&contents[written..written + to_write]);
                is_first = false;
            } else {
                page_data[4..4 + to_write].copy_from_slice(&contents[written..written + to_write]);
            }
            
            btree.write_page(current_page, &page_data);
            written += to_write;
            current_page = next_page;
        }
        
        let mut val_bytes = [0u8; 512];
        val_bytes[0..4].copy_from_slice(&first_page.to_le_bytes());
        btree.insert(key, val_bytes);
        Ok(())
    }

    fn set_permissions(&self, _path: &Path, _permissions: Permissions) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct FilesystemStub {
    exists: BTreeSet<PathBuf>,
    read_to_string: BTreeMap<PathBuf, Result<String>>,
    create_dir_all_errors: BTreeMap<PathBuf, Error>,
    write_errors: BTreeMap<PathBuf, Error>,
}

impl Filesystem for FilesystemStub {
    fn exists(&self, path: &Path) -> bool {
        self.exists.contains(&PathBuf::from(path.as_ref()))
    }

    fn read_to_string(&mut self, f: &Path) -> Result<String> {
        match self.read_to_string.remove(&PathBuf::from(f.as_ref())) {
            Some(contents) => contents,
            None => Err(Error::new(
                ErrorKind::NotFound,
                format!("no response configured for file {}", f.as_ref()),
            )),
        }
    }

    fn create_dir_all(&mut self, path: &Path) -> Result<()> {
        match self.create_dir_all_errors.remove(&PathBuf::from(path.as_ref())) {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn write(&mut self, path: &Path, _contents: &[u8]) -> Result<()> {
        match self.write_errors.remove(&PathBuf::from(path.as_ref())) {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn set_permissions(&self, _path: &Path, _permissions: Permissions) -> Result<()> {
        Ok(())
    }
}
