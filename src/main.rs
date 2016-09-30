/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this file,
 * You can obtain one at http://mozilla.org/MPL/2.0/. */

extern crate argparse;
#[macro_use]
extern crate js;
extern crate libc;
extern crate linenoise;

use std::cell::RefCell;
use std::env;
use std::ffi::CStr;
use std::fs::File;
use std::io::Read;
use std::ptr;
use std::str;

use argparse::{ArgumentParser, StoreTrue, Store};
use js::{JSCLASS_RESERVED_SLOTS_MASK,JSCLASS_GLOBAL_SLOT_COUNT,JSCLASS_IS_GLOBAL};
use js::jsapi::{CurrentGlobalOrNull, JSCLASS_RESERVED_SLOTS_SHIFT};
use js::jsapi::{CallArgs,CompartmentOptions,OnNewGlobalHookOption,Value};
use js::jsapi::{JS_DefineFunction,JS_Init,JS_NewGlobalObject, JS_InitStandardClasses,JS_EncodeStringToUTF8, JS_BufferIsCompilableUnit};
use js::jsapi::{JSAutoCompartment, JSContext, JSClass};
use js::jsapi::{JS_IsExceptionPending, JS_GetPendingException, JS_ClearPendingException, JS_ErrorFromException};
use js::jsapi::{JS_SetGCParameter, JSGCParamKey, JSGCMode, DisableIncrementalGC};
use js::jsapi::{HandleObject, HandleValue, RuntimeOptionsRef};
use js::jsapi::{JS_SetParallelParsingEnabled, JS_SetOffthreadIonCompilationEnabled, JSJitCompilerOption};
use js::jsapi::JS_SetGlobalJitCompilerOption;
use js::jsval::UndefinedValue;
use js::rust::Runtime;
use js::conversions::ToJSValConvertible;
use libc::c_uint;
use std::slice::from_raw_parts;

thread_local!(pub static RUNTIME: RefCell<Option<Runtime>> = RefCell::new(None));

static CLASS: &'static JSClass = &JSClass {
    name: b"test\0" as *const u8 as *const libc::c_char,
    flags: JSCLASS_IS_GLOBAL | ((JSCLASS_GLOBAL_SLOT_COUNT & JSCLASS_RESERVED_SLOTS_MASK) << JSCLASS_RESERVED_SLOTS_SHIFT),
    cOps: 0 as *const _,
    reserved: [0 as *mut _; 3]
};

struct JSOptions {
    interactive: bool,
    disable_baseline: bool,
    disable_ion: bool,
    disable_asmjs: bool,
    disable_native_regexp: bool,
    disable_parallel_parsing: bool,
    disable_offthread_compilation: bool,
    enable_baseline_unsafe_eager_compilation: bool,
    enable_ion_unsafe_eager_compilation: bool,
    enable_discard_system_source: bool,
    enable_asyncstack: bool,
    enable_throw_on_debugee_would_run: bool,
    enable_dump_stack_on_debugee_would_run: bool,
    enable_werror: bool,
    enable_strict: bool,
    enable_incremental: bool,
    disable_shared_memory: bool,
    disable_gc_per_compartment: bool,
    disable_compacting: bool,
    disable_dynamic_work_slice: bool,
    disable_dynamic_mark_slice: bool,
    disable_refresh_frame_slices: bool,
    disable_dynamic_heap_growth: bool,
    script: String,
}

fn main() {
    let js_options = parse_args();

    unsafe { JS_Init(); }

    let runtime = Runtime::new();
    let rt = runtime.rt();
    let cx = runtime.cx();

    let h_option = OnNewGlobalHookOption::FireOnNewGlobalHook;
    let c_option = CompartmentOptions::default();
    let global = unsafe { JS_NewGlobalObject(cx, CLASS, ptr::null_mut(), h_option, &c_option) };
    rooted!(in(cx) let global_root = global);
    let global = global_root.handle();
    let _ac = JSAutoCompartment::new(cx, global.get());

    let rt_opts = unsafe { &mut *RuntimeOptionsRef(rt) };
    rt_opts.set_baseline_(!js_options.disable_baseline);
    rt_opts.set_ion_(!js_options.disable_ion);
    rt_opts.set_asmJS_(!js_options.disable_asmjs);
    rt_opts.set_extraWarnings_(js_options.enable_strict);
    rt_opts.set_nativeRegExp_(!js_options.disable_native_regexp);
    rt_opts.set_werror_(js_options.enable_werror);

    let gc_mode = if js_options.enable_incremental {
        JSGCMode::JSGC_MODE_INCREMENTAL
    } else if !js_options.disable_gc_per_compartment {
        JSGCMode::JSGC_MODE_COMPARTMENT
    } else {
        JSGCMode::JSGC_MODE_GLOBAL
    };

    unsafe {
        // Pre barriers aren't working correctly at the moment
        DisableIncrementalGC(runtime.rt());

        JS_SetGCParameter(rt, JSGCParamKey::JSGC_MODE, gc_mode as u32);
        JS_SetGCParameter(rt, JSGCParamKey::JSGC_COMPACTING_ENABLED, !js_options.disable_compacting as u32);
        JS_SetGCParameter(rt, JSGCParamKey::JSGC_DYNAMIC_MARK_SLICE, !js_options.disable_dynamic_mark_slice as u32);
        JS_SetGCParameter(rt, JSGCParamKey::JSGC_DYNAMIC_HEAP_GROWTH, !js_options.disable_dynamic_heap_growth as u32);

        JS_SetParallelParsingEnabled(rt, !js_options.disable_parallel_parsing);
        JS_SetOffthreadIonCompilationEnabled(rt, !js_options.disable_offthread_compilation);
        JS_SetGlobalJitCompilerOption(rt, JSJitCompilerOption::JSJITCOMPILER_BASELINE_WARMUP_TRIGGER,
                                      if js_options.enable_baseline_unsafe_eager_compilation { 0i32 } else { -1i32 } as u32);
        JS_SetGlobalJitCompilerOption(rt, JSJitCompilerOption::JSJITCOMPILER_ION_WARMUP_TRIGGER,
                                      if js_options.enable_ion_unsafe_eager_compilation { 0i32 } else { -1i32 } as u32);

        JS_InitStandardClasses(cx, global);

        JS_DefineFunction(cx, global, b"print\0".as_ptr() as *const _, Some(print), 1, 0);
        JS_DefineFunction(cx, global, b"load\0".as_ptr() as *const _, Some(load), 1, 0);
        JS_DefineFunction(cx, global, b"read\0".as_ptr() as *const _, Some(read), 1, 0);
        JS_DefineFunction(cx, global, b"readFile\0".as_ptr() as *const _, Some(read), 1, 0);
    }

    if js_options.script != "" {
        let _ = run_script(&runtime, global, &js_options.script);
    }

    if js_options.script == "" || js_options.interactive {
        run_read_eval_print_loop(&runtime, global);
    }
}

fn run_read_eval_print_loop(runtime: &Runtime, global: HandleObject) {
    let mut line_no = 1u32;

    loop {
        let start_line = line_no;
        let mut buffer = String::new();
        loop {
            let line = match linenoise::prompt("js> ") {
                None => return,
                Some(line) => line
            };
            buffer.push_str(&line);
            line_no += 1;
            linenoise::history_add(&buffer);
            let script_utf8: Vec<u8> = buffer.bytes().collect();
            let script_ptr = script_utf8.as_ptr() as *const i8;
            let script_len = script_utf8.len() as usize;
            unsafe {
                if JS_BufferIsCompilableUnit(runtime.cx(), global, script_ptr, script_len) {
                    break;
                }
            }
        }

        let _are = AutoReportException { cx: runtime.cx() };
        rooted!(in(runtime.cx()) let mut rval = UndefinedValue());
        match runtime.evaluate_script(global, &buffer, "typein", start_line, rval.handle_mut()) {
            Err(_) => {},
            _ => if !rval.handle().is_undefined() {
                println!("{}", fmt_js_value(runtime.cx(), rval.handle()))
            }
        }
    }
}

fn run_script(runtime: &Runtime, global: HandleObject, filename: &String) -> Result<i32, &'static str> {
    let mut source = String::new();
    {
        let mut file = match File::open(&filename) {
            Err(_) => return Err("Error opening source file"),
            Ok(file) => file
        };
        if let Err(_) = file.read_to_string(&mut source) {
            return Err("Error reading from source file");
        }
    }

    let _are = AutoReportException { cx: runtime.cx() };
    rooted!(in(runtime.cx()) let mut rval = UndefinedValue());
    match runtime.evaluate_script(global, &source, filename, 1, rval.handle_mut()) {
        Err(_) => Err("Error executing script"),
        _ => Ok(1)
    }
}

fn parse_args() -> JSOptions {
    let mut options = JSOptions {
        interactive: false,
        disable_baseline: false,
        disable_ion: false,
        disable_asmjs: false,
        disable_native_regexp: false,
        disable_parallel_parsing: false,
        disable_offthread_compilation: false,
        enable_baseline_unsafe_eager_compilation: false,
        enable_ion_unsafe_eager_compilation: false,
        enable_discard_system_source: false,
        enable_asyncstack: false,
        enable_throw_on_debugee_would_run: false,
        enable_dump_stack_on_debugee_would_run: false,
        enable_werror: false,
        enable_strict: false,
        disable_shared_memory: false,
        disable_gc_per_compartment: false,
        enable_incremental: false,
        disable_compacting: false,
        disable_dynamic_work_slice: false,
        disable_dynamic_mark_slice: false,
        disable_refresh_frame_slices: false,
        disable_dynamic_heap_growth: false,
        script: String::new(),
    };
    {
        let mut ap = ArgumentParser::new();
        ap.set_description("The SpiderMonkey shell provides a command line interface to the
            JavaScript engine. Code and file options provided via the command line are
            run left to right. If provided, the optional script argument is run after
            all options have been processed. Just-In-Time compilation modes may be enabled via
            command line options.");
        ap.refer(&mut options.interactive)
            .add_option(&["-i", "--shell"], StoreTrue,
            "Enter prompt after running code");
        ap.refer(&mut options.disable_baseline)
            .add_option(&["--no-baseline"], StoreTrue,
            "Disable baseline compiler");
        ap.refer(&mut options.disable_ion)
            .add_option(&["--no-ion"], StoreTrue,
            "Disable IonMonkey");
        ap.refer(&mut options.disable_asmjs)
            .add_option(&["--no-asmjs"], StoreTrue,
            "Disable asm.js compilation");
        ap.refer(&mut options.disable_native_regexp)
            .add_option(&["--no-native-regexp"], StoreTrue,
                        "Disable native regexp compilation");
        ap.refer(&mut options.disable_parallel_parsing)
            .add_option(&["--no-parallel-parsing"], StoreTrue,
                        "Disable parallel parsing");
        ap.refer(&mut options.disable_offthread_compilation)
            .add_option(&["--no-offthread-compilation"], StoreTrue,
                        "Disable offthread compilation");
        ap.refer(&mut options.enable_baseline_unsafe_eager_compilation)
            .add_option(&["--baseline-unsafe-eager-compilation"], StoreTrue,
                        "Enable baseline unsafe eager compilation");
        ap.refer(&mut options.enable_ion_unsafe_eager_compilation)
            .add_option(&["--ion-unsafe-eager-compilation"], StoreTrue,
                        "Enable ion unsafe eager compilation");
        ap.refer(&mut options.enable_discard_system_source)
            .add_option(&["--discard-system-source"], StoreTrue,
                        "Enable discard system source");
        ap.refer(&mut options.enable_asyncstack)
            .add_option(&["--asyncstack"], StoreTrue,
                        "Enable asyncstack");
        ap.refer(&mut options.enable_throw_on_debugee_would_run)
            .add_option(&["--throw-on-debugee-would-run"], StoreTrue,
                        "Enable throw on debugee would run");
        ap.refer(&mut options.enable_dump_stack_on_debugee_would_run)
            .add_option(&["--dump-stack-on-debugee-would-run"], StoreTrue,
                        "Enable dump stack on debugee would run");
        ap.refer(&mut options.enable_werror)
            .add_option(&["--werror"], StoreTrue,
                        "Enable werror");
        ap.refer(&mut options.enable_strict)
            .add_option(&["--strict"], StoreTrue,
                        "Enable strict");
        ap.refer(&mut options.disable_shared_memory)
            .add_option(&["--no-shared-memory"], StoreTrue,
                        "Disable shared memory");
        ap.refer(&mut options.disable_gc_per_compartment)
            .add_option(&["--no-gc-per-compartment"], StoreTrue,
                        "Disable GC per compartment");
        ap.refer(&mut options.enable_incremental)
            .add_option(&["--incremental"], StoreTrue,
                        "Enable incremental GC");
        ap.refer(&mut options.disable_compacting)
            .add_option(&["--no-compacting"], StoreTrue,
                        "Disable compacting");
        ap.refer(&mut options.disable_dynamic_work_slice)
            .add_option(&["--no-dynamic-work-slice"], StoreTrue,
                        "Disable dynamic work slice");
        ap.refer(&mut options.disable_dynamic_mark_slice)
            .add_option(&["--no-dynamic-mark-slice"], StoreTrue,
                        "Disable dynamic mark slice");
        ap.refer(&mut options.disable_refresh_frame_slices)
            .add_option(&["--no-refresh_frame_slices"], StoreTrue,
                        "Disable refresh frame slices");
        ap.refer(&mut options.disable_dynamic_heap_growth)
            .add_option(&["--no-dynamic-heap-growth"], StoreTrue,
                        "Disable dynamic heap growth");
        ap.refer(&mut options.script)
            .add_argument("script", Store,
            "A script to execute (after all options)");
        ap.parse_args_or_exit();
    }
    options
}

unsafe extern "C" fn print(cx: *mut JSContext, argc: u32, vp: *mut Value) -> bool {
    let args = CallArgs::from_vp(vp, argc);

    let output = (0..args._base.argc_)
        .map(|i| fmt_js_value(cx, args.get(i)))
        .collect::<Vec<String>>()
        .join(" ");
    println!("{}", output);

    args.rval().set(UndefinedValue());
    return true;
}

unsafe extern "C" fn load(cx: *mut JSContext, argc: u32, vp: *mut Value) -> bool {
    let args = CallArgs::from_vp(vp, argc);
    for i in 0..args._base.argc_ {
        let val = args.get(i);
        let s = js::rust::ToString(cx, val);
        if s.is_null() {
            // report error
            return false;
        }
        let mut filename = env::current_dir().unwrap();
        rooted!(in(cx) let path_root = s);
        let path = JS_EncodeStringToUTF8(cx, path_root.handle());
        let path = CStr::from_ptr(path);
        filename.push(str::from_utf8(path.to_bytes()).unwrap());
        rooted!(in(cx) let global_root = CurrentGlobalOrNull(cx));
        RUNTIME.with(|ref r| {
            let _ = run_script(r.borrow().as_ref().unwrap(), global_root.handle(),
                               &filename.to_str().unwrap().to_owned());
        });
    }

    args.rval().set(UndefinedValue());
    return true;
}

unsafe extern "C" fn read(cx: *mut JSContext, argc: u32, vp: *mut Value) -> bool {
    if argc < 1 {
        return false;
    }

    let args = CallArgs::from_vp(vp, argc);
    let val = args.get(0);
    let s = js::rust::ToString(cx, val);
    if s.is_null() {
        // TODO: report error
        return false;
    }

    let mut filename = env::current_dir().unwrap();
    rooted!(in(cx) let path_root = s);
    let path = JS_EncodeStringToUTF8(cx, path_root.handle());
    let path = CStr::from_ptr(path);
    filename.push(str::from_utf8(path.to_bytes()).unwrap());

    let mut file = match File::open(&filename) {
        Ok(file) => file,
        _ => {
            // TODO: report error
            return false;
        }
    };

    let mut source = String::new();
    if let Err(_) = file.read_to_string(&mut source) {
        // TODO: report error
        return false;
    }

    source.to_jsval(cx, args.rval());
    return true;
}

fn fmt_js_value(cx: *mut JSContext, val: HandleValue) -> String {
    let js = unsafe { js::rust::ToString(cx, val) };
    rooted!(in(cx) let message_root = js);
    let message = unsafe { JS_EncodeStringToUTF8(cx, message_root.handle()) };
    let message = unsafe { CStr::from_ptr(message) };
    String::from_utf8_lossy(message.to_bytes()).into_owned()
}

struct AutoReportException {
    cx: *mut JSContext
}

impl Drop for AutoReportException {
    fn drop(&mut self) {
        unsafe { report_pending_exception(self.cx) };
    }
}

/// A struct encapsulating information about a runtime script error.
pub struct ErrorInfo {
    /// The error message.
    pub message: String,
    /// The file name.
    pub filename: String,
    /// The line number.
    pub lineno: c_uint,
    /// The column number.
    pub column: c_uint,
}

unsafe fn report_pending_exception(cx: *mut JSContext) {
    if !JS_IsExceptionPending(cx) {
        return;
    }
    rooted!(in(cx) let mut value = UndefinedValue());
    if !JS_GetPendingException(cx, value.handle_mut()) {
        JS_ClearPendingException(cx);
        println!("Uncaught exception: JS_GetPendingException failed");
        return;
    }

    JS_ClearPendingException(cx);

    let maybe_report = if value.is_object() {
        rooted!(in(cx) let object = value.to_object());
        let report = JS_ErrorFromException(cx, object.handle());
        if report.is_null() { None } else { Some(report) }
    } else {
        None
    };

    let error_info = match maybe_report {
        Some(report) => {
            let filename = {
                let filename = (*report).filename as *const u8;
                if !filename.is_null() {
                    let length = (0..).find(|idx| *filename.offset(*idx) == 0).unwrap();
                    let filename = from_raw_parts(filename, length as usize);
                    String::from_utf8_lossy(filename).into_owned()
                } else {
                    "none".to_string()
                }
            };

            let lineno = (*report).lineno;
            let column = (*report).column;

            let message = {
                let message = (*report).ucmessage;
                let length = (0..).find(|idx| *message.offset(*idx) == 0).unwrap();
                let message = from_raw_parts(message, length as usize);
                String::from_utf16_lossy(message)
            };

            ErrorInfo {
                filename: filename,
                message: message,
                lineno: lineno,
                column: column,
            }
        },
        None => {
            ErrorInfo {
                message: format!("Thrown value: {}", fmt_js_value(cx, value.handle())),
                filename: String::new(),
                lineno: 0,
                column: 0
            }
        }
    };

    println!("Uncaught exception at {}:{}:{} - {}",
             error_info.filename,
             error_info.lineno,
             error_info.column,
             error_info.message);
}
