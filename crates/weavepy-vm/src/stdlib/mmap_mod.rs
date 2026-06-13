//! The `mmap` module — RFC 0023.
//!
//! Memory-mapped files via the `memmap2` crate. The surface mirrors
//! CPython's `mmap.mmap` minimum: `mmap(fileno, length, access=,
//! offset=)`, with `read`, `read_byte`, `write`, `seek`, `tell`,
//! `size`, `flush`, `close`, slicing, and `find`.

use crate::sync::Rc;
use crate::sync::RefCell;

use memmap2::{Mmap, MmapMut};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeFlags, TypeObject};

pub const ACCESS_DEFAULT: i64 = 0;
pub const ACCESS_READ: i64 = 1;
pub const ACCESS_WRITE: i64 = 2;
pub const ACCESS_COPY: i64 = 3;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("mmap"),
        );
        for (n, v) in [
            ("ACCESS_DEFAULT", ACCESS_DEFAULT),
            ("ACCESS_READ", ACCESS_READ),
            ("ACCESS_WRITE", ACCESS_WRITE),
            ("ACCESS_COPY", ACCESS_COPY),
            // POSIX MAP_* constants from Python.
            ("MAP_SHARED", 0x01),
            ("MAP_PRIVATE", 0x02),
            ("MAP_ANONYMOUS", 0x20),
            ("PROT_READ", 0x01),
            ("PROT_WRITE", 0x02),
            ("PROT_EXEC", 0x04),
        ] {
            d.insert(DictKey(Object::from_static(n)), Object::Int(v));
        }
        d.insert(
            DictKey(Object::from_static("mmap")),
            Object::Type(mmap_type()),
        );
        d.insert(
            DictKey(Object::from_static("error")),
            Object::Type(crate::builtin_types::builtin_types().os_error.clone()),
        );
    }
    Rc::new(PyModule {
        name: "mmap".to_owned(),
        filename: None,
        dict,
    })
}

fn mmap_type() -> Rc<TypeObject> {
    use crate::builtin_types::builtin_types;
    let bt = builtin_types();
    let mut td = DictData::new();
    for (name, fn_) in [
        (
            "__init__",
            mm_init as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("read", mm_read),
        ("read_byte", mm_read_byte),
        ("readline", mm_readline),
        ("write", mm_write),
        ("write_byte", mm_write_byte),
        ("seek", mm_seek),
        ("tell", mm_tell),
        ("size", mm_size),
        ("flush", mm_flush),
        ("close", mm_close),
        ("find", mm_find),
        ("rfind", mm_rfind),
        ("__enter__", mm_enter),
        ("__exit__", mm_exit),
        ("__len__", mm_size),
    ] {
        td.insert(
            DictKey(Object::from_static(name)),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: true,
                call: Box::new(fn_),
                call_kw: None,
            })),
        );
    }
    TypeObject::new_with_flags(
        "mmap",
        vec![bt.object_.clone()],
        td,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("mmap.mmap")
}

enum MmapBacking {
    Read(Mmap),
    Write(MmapMut),
}

struct MmapState {
    backing: MmapBacking,
    pos: usize,
}

thread_local! {
    static MMAP_STATE: RefCell<std::collections::HashMap<usize, RefCell<MmapState>>> =
        RefCell::new(std::collections::HashMap::new());
    static MMAP_NEXT_ID: RefCell<usize> = const { RefCell::new(1) };
}

fn alloc_state(state: MmapState) -> usize {
    let id = MMAP_NEXT_ID.with(|n| {
        let mut g = n.borrow_mut();
        let id = *g;
        *g += 1;
        id
    });
    MMAP_STATE.with(|m| m.borrow_mut().insert(id, RefCell::new(state)));
    id
}

fn with_state<R>(
    inst: &Rc<PyInstance>,
    f: impl FnOnce(&mut MmapState) -> R,
) -> Result<R, RuntimeError> {
    let id = state_id(inst)?;
    MMAP_STATE.with(|m| {
        let map = m.borrow();
        let cell = map.get(&id).ok_or_else(|| value_error("mmap: closed"))?;
        let mut state = cell.borrow_mut();
        Ok(f(&mut state))
    })
}

fn state_id(inst: &Rc<PyInstance>) -> Result<usize, RuntimeError> {
    match inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_id")))
        .cloned()
    {
        Some(Object::Int(i)) if i > 0 => Ok(i as usize),
        _ => Err(value_error("mmap: closed")),
    }
}

fn mm_init(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("mmap.__init__: missing self")),
    };
    let fileno = match args.get(1) {
        Some(Object::Int(i)) => *i,
        _ => return Err(type_error("mmap: fileno must be int")),
    };
    let length = match args.get(2) {
        Some(Object::Int(i)) => *i as usize,
        _ => return Err(type_error("mmap: length must be int")),
    };
    let access = match args.get(3) {
        Some(Object::Int(i)) => *i,
        _ => ACCESS_DEFAULT,
    };
    if fileno == -1 {
        // Anonymous mapping.
        let map = MmapMut::map_anon(length)
            .map_err(|e| crate::error::os_error(format!("mmap_anon: {e}")))?;
        let id = alloc_state(MmapState {
            backing: MmapBacking::Write(map),
            pos: 0,
        });
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("_id")), Object::Int(id as i64));
        return Ok(Object::None);
    }
    // SAFETY: we trust the caller to pass a live file descriptor
    // (Unix) / OS HANDLE (Windows, as returned by
    // `msvcrt._get_osfhandle(fd)`). `ManuallyDrop` keeps the
    // underlying fd/handle alive past this function — closing it is
    // the caller's responsibility.
    let file = file_from_fileno(fileno);
    let file_ref = std::mem::ManuallyDrop::new(file);
    let backing = match access {
        ACCESS_READ => {
            let map = unsafe { Mmap::map(&*file_ref) }
                .map_err(|e| crate::error::os_error(format!("mmap: {e}")))?;
            MmapBacking::Read(map)
        }
        _ => {
            let map = unsafe { MmapMut::map_mut(&*file_ref) }
                .map_err(|e| crate::error::os_error(format!("mmap: {e}")))?;
            MmapBacking::Write(map)
        }
    };
    let id = alloc_state(MmapState { backing, pos: 0 });
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_id")), Object::Int(id as i64));
    Ok(Object::None)
}

fn mmap_bytes(state: &MmapState) -> &[u8] {
    match &state.backing {
        MmapBacking::Read(m) => &m[..],
        MmapBacking::Write(m) => &m[..],
    }
}

fn mmap_bytes_mut(state: &mut MmapState) -> Option<&mut [u8]> {
    match &mut state.backing {
        MmapBacking::Write(m) => Some(&mut m[..]),
        MmapBacking::Read(_) => None,
    }
}

fn mm_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let n = match args.get(1) {
        Some(Object::Int(n)) => Some(*n as usize),
        Some(Object::None) | None => None,
        _ => return Err(type_error("read: n must be int or None")),
    };
    with_state(&inst, |s| {
        let buf = mmap_bytes(s);
        let end = match n {
            Some(k) => (s.pos + k).min(buf.len()),
            None => buf.len(),
        };
        let result = buf[s.pos..end].to_vec();
        s.pos = end;
        Object::Bytes(Rc::from(result.into_boxed_slice()))
    })
}

fn mm_read_byte(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    with_state(&inst, |s| {
        let buf = mmap_bytes(s);
        if s.pos >= buf.len() {
            return Object::Int(-1);
        }
        let b = buf[s.pos];
        s.pos += 1;
        Object::Int(i64::from(b))
    })
}

fn mm_readline(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    with_state(&inst, |s| {
        let start = s.pos;
        let line: Vec<u8> = {
            let buf = mmap_bytes(s);
            let mut end = start;
            while end < buf.len() {
                if buf[end] == b'\n' {
                    end += 1;
                    break;
                }
                end += 1;
            }
            let v = buf[start..end].to_vec();
            s.pos = end;
            v
        };
        Object::Bytes(Rc::from(line.into_boxed_slice()))
    })
}

fn mm_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let data: Vec<u8> = match args.get(1) {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        _ => return Err(type_error("write: argument must be bytes-like")),
    };
    with_state(&inst, |s| {
        let pos = s.pos;
        let needed = pos + data.len();
        let written = if let Some(buf) = mmap_bytes_mut(s) {
            if needed > buf.len() {
                return Err(value_error("mmap: write beyond end of mapping"));
            }
            buf[pos..pos + data.len()].copy_from_slice(&data);
            data.len()
        } else {
            return Err(value_error("mmap: not writable"));
        };
        s.pos += written;
        Ok(Object::Int(written as i64))
    })?
}

fn mm_write_byte(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let b = match args.get(1) {
        Some(Object::Int(i)) if (0..=255).contains(i) => *i as u8,
        _ => return Err(value_error("write_byte: byte out of range")),
    };
    with_state(&inst, |s| {
        let pos = s.pos;
        let _ok = if let Some(buf) = mmap_bytes_mut(s) {
            if pos >= buf.len() {
                return Err(value_error("mmap: write_byte beyond end of mapping"));
            }
            buf[pos] = b;
            true
        } else {
            return Err(value_error("mmap: not writable"));
        };
        s.pos += 1;
        Ok(Object::None)
    })?
}

fn mm_seek(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let off = match args.get(1) {
        Some(Object::Int(i)) => *i,
        _ => return Err(type_error("seek: offset must be int")),
    };
    let whence = match args.get(2) {
        Some(Object::Int(i)) => *i,
        None => 0,
        _ => return Err(type_error("seek: whence must be int")),
    };
    with_state(&inst, |s| {
        let len = mmap_bytes(s).len() as i64;
        let new = match whence {
            0 => off,
            1 => s.pos as i64 + off,
            2 => len + off,
            _ => return Err(value_error("seek: invalid whence")),
        };
        if new < 0 || new > len {
            return Err(value_error("seek out of range"));
        }
        s.pos = new as usize;
        Ok(Object::None)
    })?
}

fn mm_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    with_state(&inst, |s| Object::Int(s.pos as i64))
}

fn mm_size(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    with_state(&inst, |s| Object::Int(mmap_bytes(s).len() as i64))
}

fn mm_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    with_state(&inst, |s| match &mut s.backing {
        MmapBacking::Write(m) => {
            let _ = m.flush();
            Object::None
        }
        MmapBacking::Read(_) => Object::None,
    })
}

fn mm_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    if let Ok(id) = state_id(&inst) {
        MMAP_STATE.with(|m| {
            m.borrow_mut().remove(&id);
        });
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("_id")), Object::Int(0));
    }
    Ok(Object::None)
}

fn mm_find(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let needle = match args.get(1) {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        _ => return Err(type_error("find: argument must be bytes-like")),
    };
    with_state(&inst, |s| {
        let buf = mmap_bytes(s);
        let start = s.pos;
        if needle.is_empty() {
            return Object::Int(start as i64);
        }
        for i in start..=buf.len().saturating_sub(needle.len()) {
            if buf[i..i + needle.len()] == needle[..] {
                return Object::Int(i as i64);
            }
        }
        Object::Int(-1)
    })
}

fn mm_rfind(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let needle = match args.get(1) {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        _ => return Err(type_error("rfind: argument must be bytes-like")),
    };
    with_state(&inst, |s| {
        let buf = mmap_bytes(s);
        if needle.is_empty() || buf.len() < needle.len() {
            return Object::Int(-1);
        }
        for i in (0..=buf.len() - needle.len()).rev() {
            if buf[i..i + needle.len()] == needle[..] {
                return Object::Int(i as i64);
            }
        }
        Object::Int(-1)
    })
}

fn mm_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(args[0].clone())
}

fn mm_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    mm_close(args)
}

fn self_arg(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error("mmap method: missing self")),
    }
}

#[cfg(unix)]
fn file_from_fileno(fileno: i64) -> std::fs::File {
    use std::os::unix::io::FromRawFd;
    // SAFETY: caller must pass an open fd; the returned File is
    // wrapped in `ManuallyDrop` by the caller so the fd is not
    // closed here.
    unsafe { std::fs::File::from_raw_fd(fileno as i32) }
}

#[cfg(windows)]
fn file_from_fileno(fileno: i64) -> std::fs::File {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    // On Windows the integer passed in is the underlying OS HANDLE
    // (as produced by `msvcrt._get_osfhandle(fd)` on the Python side).
    // SAFETY: caller must pass a live handle; `ManuallyDrop` keeps it
    // alive past this function.
    unsafe { std::fs::File::from_raw_handle(fileno as isize as RawHandle) }
}
