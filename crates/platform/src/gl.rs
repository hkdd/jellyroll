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
type FnEnable = unsafe extern "C" fn(GLenum);
type FnColorMask = unsafe extern "C" fn(u8, u8, u8, u8);
type FnBindFramebuffer = unsafe extern "C" fn(GLenum, GLuint);
type FnBindBuffer = unsafe extern "C" fn(GLenum, GLuint);
type FnBindVertexArray = unsafe extern "C" fn(GLuint);
type FnUseProgram = unsafe extern "C" fn(GLuint);
type FnActiveTexture = unsafe extern "C" fn(GLenum);
type FnBindTexture = unsafe extern "C" fn(GLenum, GLuint);
type FnPixelStorei = unsafe extern "C" fn(GLenum, GLint);
type FnGetError = unsafe extern "C" fn() -> GLenum;
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

/// Entry points for [`Gl::reset_to_defaults`].
struct ResetFns {
    disable: FnEnable,
    color_mask: FnColorMask,
    bind_framebuffer: FnBindFramebuffer,
    bind_buffer: FnBindBuffer,
    bind_vertex_array: FnBindVertexArray,
    use_program: FnUseProgram,
    active_texture: FnActiveTexture,
    bind_texture: FnBindTexture,
    pixel_storei: FnPixelStorei,
    get_error: FnGetError,
}

pub struct Gl {
    get_string: FnGetString,
    get_integerv: FnGetIntegerv,
    reset: ResetFns,
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
            reset: ResetFns {
                disable: req!("glDisable"),
                color_mask: req!("glColorMask"),
                bind_framebuffer: req!("glBindFramebuffer"),
                bind_buffer: req!("glBindBuffer"),
                bind_vertex_array: req!("glBindVertexArray"),
                use_program: req!("glUseProgram"),
                active_texture: req!("glActiveTexture"),
                bind_texture: req!("glBindTexture"),
                pixel_storei: req!("glPixelStorei"),
                get_error: req!("glGetError"),
            },
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

    /// Puts the GL state that other renderers commonly leave behind back to
    /// OpenGL defaults. libmpv's render API requires default state except
    /// viewport, scissor box, blend func and clear colour (render_gl.h), and
    /// Skia leaves blending, scissoring, bound buffers and programs set
    /// after a flush (spec 6.3).
    pub fn reset_to_defaults(&self) {
        const BLEND: GLenum = 0x0BE2;
        const SCISSOR_TEST: GLenum = 0x0C11;
        const STENCIL_TEST: GLenum = 0x0B90;
        const DEPTH_TEST: GLenum = 0x0B71;
        const CULL_FACE: GLenum = 0x0B44;
        const FRAMEBUFFER_SRGB: GLenum = 0x8DB9;
        const FRAMEBUFFER: GLenum = 0x8D40;
        const ARRAY_BUFFER: GLenum = 0x8892;
        const PIXEL_PACK_BUFFER: GLenum = 0x88EB;
        const PIXEL_UNPACK_BUFFER: GLenum = 0x88EC;
        const TEXTURE0: GLenum = 0x84C0;
        const TEXTURE_2D: GLenum = 0x0DE1;
        const UNPACK_ALIGNMENT: GLenum = 0x0CF5;
        const UNPACK_ROW_LENGTH: GLenum = 0x0CF2;
        const PACK_ALIGNMENT: GLenum = 0x0D05;
        let r = &self.reset;
        // SAFETY: context current; plain value arguments; every name is a
        // valid default (0) or a core enum for this context type.
        unsafe {
            for cap in [BLEND, SCISSOR_TEST, STENCIL_TEST, DEPTH_TEST, CULL_FACE] {
                (r.disable)(cap);
            }
            if !self.is_gles {
                (r.disable)(FRAMEBUFFER_SRGB);
            }
            (r.color_mask)(1, 1, 1, 1);
            (r.bind_framebuffer)(FRAMEBUFFER, 0);
            (r.bind_vertex_array)(0);
            (r.bind_buffer)(ARRAY_BUFFER, 0);
            (r.bind_buffer)(PIXEL_PACK_BUFFER, 0);
            (r.bind_buffer)(PIXEL_UNPACK_BUFFER, 0);
            (r.use_program)(0);
            (r.active_texture)(TEXTURE0);
            (r.bind_texture)(TEXTURE_2D, 0);
            (r.pixel_storei)(UNPACK_ALIGNMENT, 4);
            (r.pixel_storei)(UNPACK_ROW_LENGTH, 0);
            (r.pixel_storei)(PACK_ALIGNMENT, 4);
            // GL errors are sticky until read. Clear any left by the previous
            // user so the next one (mpv checks after its own calls) is not
            // blamed for them.
            for _ in 0..8 {
                let e = (r.get_error)();
                if e == 0 {
                    break;
                }
                tracing::debug!(error = format!("{e:#06x}"), "stale GL error cleared");
            }
        }
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
