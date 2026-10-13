//! Minimal hand-rolled OpenGL loader.
//!
//! The app itself only needs context info and GPU timer queries (drawing goes
//! through Skia), so this avoids a generated binding crate. Skia and mpv
//! (M3) resolve their own symbols through the platform's `gl_proc_address`.
//!
//! Every function here is a thin safe wrapper: the context is guaranteed
//! current on the render thread for the lifetime of [`Gl`] (it is created by
//! and owned by the platform, which is `!Send`).

use std::ffi::{CStr, c_char, c_void};
use std::marker::PhantomData;

pub type GLenum = u32;
pub type GLuint = u32;
pub type GLint = i32;
pub type GLsizei = i32;

const VENDOR: GLenum = 0x1F00;
const RENDERER: GLenum = 0x1F01;
const VERSION: GLenum = 0x1F02;
const SHADING_LANGUAGE_VERSION: GLenum = 0x8B8C;
const TIME_ELAPSED: GLenum = 0x88BF;
const QUERY_RESULT: GLenum = 0x8866;
const QUERY_RESULT_AVAILABLE: GLenum = 0x8867;
const GPU_DISJOINT_EXT: GLenum = 0x8FBB;

type FnGetString = unsafe extern "C" fn(GLenum) -> *const u8;
type FnGetIntegerv = unsafe extern "C" fn(GLenum, *mut GLint);
type FnGenQueries = unsafe extern "C" fn(GLsizei, *mut GLuint);
type FnDeleteQueries = unsafe extern "C" fn(GLsizei, *const GLuint);
type FnBeginQuery = unsafe extern "C" fn(GLenum, GLuint);
type FnEndQuery = unsafe extern "C" fn(GLenum);
type FnGetQueryObjectuiv = unsafe extern "C" fn(GLuint, GLenum, *mut GLuint);
type FnGetQueryObjectui64v = unsafe extern "C" fn(GLuint, GLenum, *mut u64);

struct TimerFns {
    gen_queries: FnGenQueries,
    delete: FnDeleteQueries,
    begin: FnBeginQuery,
    end: FnEndQuery,
    get_uiv: FnGetQueryObjectuiv,
    get_ui64v: FnGetQueryObjectui64v,
    /// GLES (EXT_disjoint_timer_query) can report a disjoint event that
    /// invalidates in-flight results.
    check_disjoint: bool,
}

pub struct Gl {
    get_string: FnGetString,
    get_integerv: FnGetIntegerv,
    timer: Option<TimerFns>,
    pub is_gles: bool,
    // Raw function pointers already make this !Send/!Sync; keep it explicit.
    _not_send: PhantomData<*const ()>,
}

/// Resolves a symbol to a raw pointer (null when missing).
pub type Loader<'a> = dyn Fn(&str) -> *const c_void + 'a;

fn load<T: Copy>(loader: &Loader<'_>, names: &[&str]) -> Option<T> {
    debug_assert_eq!(size_of::<T>(), size_of::<*const c_void>());
    for name in names {
        let p = loader(name);
        if !p.is_null() {
            // SAFETY: `T` is always one of the `unsafe extern "C" fn` aliases
            // above, which are pointer-sized; the symbol was resolved by the
            // GL implementation for exactly this name.
            return Some(unsafe { std::mem::transmute_copy::<*const c_void, T>(&p) });
        }
    }
    None
}

impl Gl {
    /// Loads the entry points. `has_ext` answers extension queries for the
    /// current context (needed to decide whether timer queries are usable).
    pub fn load(
        loader: &Loader<'_>,
        is_gles: bool,
        has_ext: &dyn Fn(&str) -> bool,
    ) -> Result<Self, String> {
        macro_rules! req {
            ($name:literal) => {
                load(loader, &[$name]).ok_or_else(|| format!("missing GL symbol {}", $name))?
            };
        }
        let timer = if is_gles {
            if has_ext("GL_EXT_disjoint_timer_query") {
                Some((
                    [
                        "glGenQueriesEXT",
                        "glDeleteQueriesEXT",
                        "glBeginQueryEXT",
                        "glEndQueryEXT",
                        "glGetQueryObjectuivEXT",
                        "glGetQueryObjectui64vEXT",
                    ],
                    true,
                ))
            } else {
                None
            }
        } else {
            Some((
                [
                    "glGenQueries",
                    "glDeleteQueries",
                    "glBeginQuery",
                    "glEndQuery",
                    "glGetQueryObjectuiv",
                    "glGetQueryObjectui64v",
                ],
                false,
            ))
        };
        let timer = timer.and_then(|(n, check_disjoint)| {
            Some(TimerFns {
                gen_queries: load(loader, &[n[0]])?,
                delete: load(loader, &[n[1]])?,
                begin: load(loader, &[n[2]])?,
                end: load(loader, &[n[3]])?,
                get_uiv: load(loader, &[n[4]])?,
                get_ui64v: load(loader, &[n[5]])?,
                check_disjoint,
            })
        });
        Ok(Self {
            get_string: req!("glGetString"),
            get_integerv: req!("glGetIntegerv"),
            timer,
            is_gles,
            _not_send: PhantomData,
        })
    }

    fn string(&self, name: GLenum) -> String {
        // SAFETY: context is current; glGetString returns a static
        // NUL-terminated string or null.
        unsafe {
            let p = (self.get_string)(name);
            if p.is_null() {
                return String::new();
            }
            CStr::from_ptr(p as *const c_char).to_string_lossy().into_owned()
        }
    }

    pub fn vendor(&self) -> String {
        self.string(VENDOR)
    }
    pub fn renderer(&self) -> String {
        self.string(RENDERER)
    }
    pub fn version(&self) -> String {
        self.string(VERSION)
    }
    pub fn glsl_version(&self) -> String {
        self.string(SHADING_LANGUAGE_VERSION)
    }

    pub fn has_timer_queries(&self) -> bool {
        self.timer.is_some()
    }

    fn disjoint(&self) -> bool {
        let mut v: GLint = 0;
        // SAFETY: context current; GPU_DISJOINT_EXT is valid when the EXT
        // timer query extension is present (checked by caller).
        unsafe { (self.get_integerv)(GPU_DISJOINT_EXT, &mut v) };
        v != 0
    }
}

/// Ring of GL_TIME_ELAPSED queries, read back without stalling the pipeline:
/// a result is collected only once the GPU reports it available, several
/// frames later.
pub struct GpuTimer {
    queries: [GLuint; Self::DEPTH],
    pending: [bool; Self::DEPTH],
    next: usize,
    active: bool,
}

impl GpuTimer {
    const DEPTH: usize = 4;

    pub fn new(gl: &Gl) -> Option<Self> {
        let t = gl.timer.as_ref()?;
        let mut queries = [0; Self::DEPTH];
        // SAFETY: context current; buffer holds DEPTH names.
        unsafe { (t.gen_queries)(Self::DEPTH as GLsizei, queries.as_mut_ptr()) };
        Some(Self { queries, pending: [false; Self::DEPTH], next: 0, active: false })
    }

    /// Starts timing this frame's GPU work. Skips (returns false) if the slot
    /// still holds an unread result, so we never block on the GPU.
    pub fn begin(&mut self, gl: &Gl) -> bool {
        let Some(t) = gl.timer.as_ref() else { return false };
        if self.pending[self.next] {
            return false;
        }
        // SAFETY: context current; query name was generated by `new`.
        unsafe { (t.begin)(TIME_ELAPSED, self.queries[self.next]) };
        self.active = true;
        true
    }

    pub fn end(&mut self, gl: &Gl) {
        let Some(t) = gl.timer.as_ref() else { return };
        if !self.active {
            return;
        }
        // SAFETY: context current; a TIME_ELAPSED query is active.
        unsafe { (t.end)(TIME_ELAPSED) };
        self.active = false;
        self.pending[self.next] = true;
        self.next = (self.next + 1) % Self::DEPTH;
    }

    /// Calls `f(nanoseconds)` for every completed query, oldest first.
    pub fn collect(&mut self, gl: &Gl, mut f: impl FnMut(u64)) {
        let Some(t) = gl.timer.as_ref() else { return };
        let disjoint = t.check_disjoint && gl.disjoint();
        for i in 0..Self::DEPTH {
            let slot = (self.next + i) % Self::DEPTH;
            if !self.pending[slot] {
                continue;
            }
            let mut avail: GLuint = 0;
            // SAFETY: context current; query has ended.
            unsafe { (t.get_uiv)(self.queries[slot], QUERY_RESULT_AVAILABLE, &mut avail) };
            if avail == 0 {
                break; // later queries cannot be ready before earlier ones
            }
            let mut ns: u64 = 0;
            // SAFETY: as above; result is available so this does not stall.
            unsafe { (t.get_ui64v)(self.queries[slot], QUERY_RESULT, &mut ns) };
            self.pending[slot] = false;
            if !disjoint {
                f(ns);
            }
        }
    }

    pub fn destroy(self, gl: &Gl) {
        if let Some(t) = gl.timer.as_ref() {
            // SAFETY: context current; names were generated by `new`.
            unsafe { (t.delete)(Self::DEPTH as GLsizei, self.queries.as_ptr()) };
        }
    }
}
