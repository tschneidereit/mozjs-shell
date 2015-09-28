/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this file,
 * You can obtain one at http://mozilla.org/MPL/2.0/. */

extern crate argparse;
extern crate js;
extern crate libc;
extern crate linenoise;

use std::ffi::CStr;
use std::fs::File;
use std::ptr;
use std::str;
use std::io::Read;

use argparse::{ArgumentParser, StoreTrue, Store};
use js::{JSCLASS_RESERVED_SLOTS_MASK,JSCLASS_RESERVED_SLOTS_SHIFT,JSCLASS_GLOBAL_SLOT_COUNT,JSCLASS_IS_GLOBAL};
use js::jsapi::JS_GlobalObjectTraceHook;
use js::jsapi::{CallArgs,CompartmentOptions,OnNewGlobalHookOption,Rooted,Value};
use js::jsapi::{JS_DefineFunction,JS_Init,JS_NewGlobalObject, JS_InitStandardClasses,JS_EncodeStringToUTF8, JS_ReportPendingException, JS_BufferIsCompilableUnit};
use js::jsapi::{JSAutoCompartment,JSContext,JSClass};
use js::jsapi::{JS_SetGCParameter, JSGCParamKey, JSGCMode};
// use jsapi::{Rooted, RootedValue, Handle, MutableHandle};
// use jsapi::{MutableHandleValue, HandleValue, HandleObject};
use js::jsapi::{RootedValue, HandleObject, HandleValue};
use js::jsval::UndefinedValue;
use js::rust::Runtime;

static CLASS: &'static JSClass = &JSClass {
    name: b"test\0" as *const u8 as *const libc::c_char,
    flags: JSCLASS_IS_GLOBAL | ((JSCLASS_GLOBAL_SLOT_COUNT & JSCLASS_RESERVED_SLOTS_MASK) << JSCLASS_RESERVED_SLOTS_SHIFT),
    addProperty: None,
    delProperty: None,
    getProperty: None,
    setProperty: None,
    enumerate: None,
    resolve: None,
    convert: None,
    finalize: None,
    call: None,
    hasInstance: None,
    construct: None,
    trace: Some(JS_GlobalObjectTraceHook),
    reserved: [0 as *mut _; 25]
};

struct JSOptions {
    interactive: bool,
    disable_baseline: bool,
    disable_ion: bool,
    disable_asmjs: bool,
    disable_native_regexp: bool,
    script: String,
}

fn main() {
    let js_options = parse_args();

    unsafe {
        JS_Init();
    }

    let runtime = Runtime::new();
    let cx = runtime.cx();

    let h_option = OnNewGlobalHookOption::FireOnNewGlobalHook;
    let c_option = CompartmentOptions::default();
    let global = unsafe { JS_NewGlobalObject(cx, CLASS, ptr::null_mut(), h_option, &c_option) };
    let global_root = Rooted::new(cx, global);
    let global = global_root.handle();
    let _ac = JSAutoCompartment::new(cx, global.get());

    unsafe {
        JS_SetGCParameter(runtime.rt(), JSGCParamKey::JSGC_MODE, JSGCMode::JSGC_MODE_INCREMENTAL as u32);
        JS_InitStandardClasses(cx, global);
        JS_DefineFunction(cx, global, b"print\0".as_ptr() as *const libc::c_char, Some(print), 1, 0);
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
            let script_len = script_utf8.len() as u64;
            unsafe {
                if JS_BufferIsCompilableUnit(runtime.cx(), global, script_ptr, script_len) {
                    break;
                }
            }
        }
        let mut rval = RootedValue::new(runtime.cx(), UndefinedValue());
        match runtime.evaluate_script(global, &buffer, "typein", start_line, rval.handle_mut()) {
            Err(_) => unsafe { JS_ReportPendingException(runtime.cx()); },
            _ => if !rval.handle().is_undefined() {
                println!("{}", fmt_js_value(runtime.cx(), rval.handle()))
            }
        }
    }
}

fn run_script(runtime: &Runtime, global: HandleObject, filename: &String) -> Result<i32, &'static str> {
    let mut source = "".to_string();
    {
        let mut file = match File::open(&filename) {
            Err(_) => return Err("Error opening source file"),
            Ok(file) => file
        };
        if let Err(_) = file.read_to_string(&mut source) {
            return Err("Error reading from source file");
        }
    }
    let mut rval = RootedValue::new(runtime.cx(), UndefinedValue());
    match runtime.evaluate_script(global, &source, filename, 1, rval.handle_mut()) {
        Err(_) => unsafe { JS_ReportPendingException(runtime.cx()); Err("Uncaught exception during script execution") },
        _ => Ok(1)
    }
}

fn parse_args<'a>() -> JSOptions {
    let mut options = JSOptions {
        interactive: false,
        disable_baseline: false,
        disable_ion: false,
        disable_asmjs: false,
        disable_native_regexp: false,
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

fn fmt_js_value(cx: *mut JSContext, val: HandleValue) -> String {
    let js = js::rust::ToString(cx, val);
    let message_root = Rooted::new(cx, js);
    let message = unsafe { JS_EncodeStringToUTF8(cx, message_root.handle()) };
    let message = unsafe { CStr::from_ptr(message) };
    String::from(str::from_utf8(message.to_bytes()).unwrap())
}
