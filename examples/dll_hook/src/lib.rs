use std::{ffi::c_void, path::PathBuf};

use anyhow::{anyhow, Context, Result};
use patternsleuth::resolvers::impl_try_collector;
use patternsleuth::resolvers::unreal::*;
use simple_log::{error, info, LogConfigBuilder};
use windows::Win32::{
    Foundation::HMODULE,
    System::{
        SystemServices::*,
        Threading::{GetCurrentThread, QueueUserAPC},
    },
};

// x3daudio1_7.dll
#[no_mangle]
#[allow(non_snake_case, unused_variables)]
extern "system" fn X3DAudioCalculate() {}
#[no_mangle]
#[allow(non_snake_case, unused_variables)]
extern "system" fn X3DAudioInitialize() {}

// d3d9.dll
#[no_mangle]
#[allow(non_snake_case, unused_variables)]
extern "system" fn D3DPERF_EndEvent() {}
#[no_mangle]
#[allow(non_snake_case, unused_variables)]
extern "system" fn D3DPERF_BeginEvent() {}

// d3d11.dll
#[no_mangle]
#[allow(non_snake_case, unused_variables)]
extern "system" fn D3D11CreateDevice() {}

// dxgi.dll
#[no_mangle]
#[allow(non_snake_case, unused_variables)]
extern "system" fn CreateDXGIFactory() {}
#[no_mangle]
#[allow(non_snake_case, unused_variables)]
extern "system" fn CreateDXGIFactory1() {}

#[no_mangle]
#[allow(non_snake_case, unused_variables)]
extern "system" fn DllMain(dll_module: HMODULE, call_reason: u32, _: *mut ()) -> bool {
    unsafe {
        match call_reason {
            DLL_PROCESS_ATTACH => {
                QueueUserAPC(Some(init), GetCurrentThread(), 0);
            }
            DLL_PROCESS_DETACH => (),
            _ => (),
        }

        true
    }
}

unsafe extern "system" fn init(_: usize) {
    if let Ok(bin_dir) = setup() {
        info!("dll_hook loaded",);

        if let Err(e) = patch(bin_dir) {
            error!("{e:#}");
        }
    }
}

fn setup() -> Result<PathBuf> {
    let exe_path = std::env::current_exe()?;
    let bin_dir = exe_path.parent().context("could not find exe parent dir")?;
    let config = LogConfigBuilder::builder()
        .path(bin_dir.join("dll_hook.txt").to_str().unwrap()) // TODO why does this not take a path??
        .time_format("%Y-%m-%d %H:%M:%S.%f")
        .level("debug")
        .output_file()
        .size(u64::MAX)
        .build();
    simple_log::new(config).map_err(|e| anyhow!("{e}"))?;
    Ok(bin_dir.to_path_buf())
}

#[derive(Debug)]
pub struct StartRecordingReplay(usize);
type FnStartRecordingReplay = unsafe extern "system" fn(
    this: *const ue::UObject, // game instance
    name: &ue::FString,
    friendly_name: &ue::FString,
    additional_options: &ue::TArray<ue::FString>,
    analytics_provider: ue::TSharedPtr<c_void>,
);
impl StartRecordingReplay {
    fn get(&self) -> FnStartRecordingReplay {
        unsafe { std::mem::transmute(self.0) }
    }
}

#[derive(Debug)]
pub struct StopRecordingReplay(usize);
type FnStopRecordingReplay = unsafe extern "system" fn(
    this: *const ue::UObject, // game instance
);
impl StopRecordingReplay {
    fn get(&self) -> FnStopRecordingReplay {
        unsafe { std::mem::transmute(self.0) }
    }
}

mod resolvers {
    use crate::{StartRecordingReplay, StopRecordingReplay};

    use patternsleuth::{
        resolvers::{futures::future::join_all, *},
        scanner::Pattern,
    };

    impl_resolver!(StartRecordingReplay, |ctx| async {
        // public: virtual void __cdecl UGameInstance::StartRecordingReplay(class FString const &, class FString const &, class TArray<class FString, class TSizedDefaultAllocator<32> > const &, class TSharedPtr<class IAnalyticsProvider, 0>)
        let patterns = [
            "48 89 5C 24 08 48 89 6C 24 10 48 89 74 24 18 48 89 7C 24 20 41 56 48 83 EC 40 49 8B F1 49 8B E8 4C 8B F2 48 8B F9 E8 ?? ?? ?? 00 48 8B D8 48 85 C0 74 24 E8 ?? ?? ?? 00 48 85 C0 74 1A 4C 8D 48 ?? 48 63 40 ?? 3B 43 ?? 7F 0D 48 8B C8 48 8B 43 ?? 4C 39 0C C8 74 02 33 DB 48 8D 8F ?? 00 00 00 48 8B D3 E8"
        ];

        let res = join_all(patterns.iter().map(|p| ctx.scan(Pattern::new(p).unwrap()))).await;

        Ok(StartRecordingReplay(ensure_one(res.into_iter().flatten())?))
    });

    impl_resolver!(StopRecordingReplay, |ctx| async {
        // public: virtual void __cdecl UGameInstance::StopRecordingReplay(void)
        let patterns = [
            "48 89 5C 24 08 57 48 83 EC 20 48 8B F9 E8 ?? ?? ?? 00 48 8B D8 48 85 C0 74 24 E8 ?? ?? ?? 00 48 85 C0 74 1A 48 8D 50 ?? 48 63 40 ?? 3B 43 ?? 7F 0D 48 8B C8 48 8B 43 ?? 48 39 14 C8 74 02 33 DB 48 8D 8F ?? 00 00 00 48 8B D3 E8 ?? ?? ?? 00 48 85 C0 74 08 48 8B C8 E8 ?? ?? ?? 00 48 8B 5C 24 30 48 83 C4"
        ];

        let res = join_all(patterns.iter().map(|p| ctx.scan(Pattern::new(p).unwrap()))).await;

        Ok(StopRecordingReplay(ensure_one(res.into_iter().flatten())?))
    });
}

impl_try_collector! {
    #[derive(Debug)]
    struct DllHookResolution {
        start_recording_replay: StartRecordingReplay,
        stop_recording_replay: StopRecordingReplay,
        gmalloc: GMalloc,
        guobject_array: GUObjectArray,
        fnametostring: FNameToString,
        allocate_uobject: FUObjectArrayAllocateUObjectIndex,
        free_uobject: FUObjectArrayFreeUObjectIndex,
        game_tick: UGameEngineTick,
        kismet_system_library: KismetSystemLibrary,
        fframe_step_via_exec: FFrameStepViaExec,
        fframe_step: FFrameStep,
        fframe_step_explicit_property: FFrameStepExplicitProperty,
    }
}

retour::static_detour! {
    static HookUGameEngineTick: unsafe extern "system" fn(*mut c_void, f32, u8);
    static HookAllocateUObject: unsafe extern "system" fn(*mut c_void, *const c_void, bool);
    static HookFreeUObject: unsafe extern "system" fn(*mut c_void, *const c_void);
    static HookKismetPrintString: unsafe extern "system" fn(*mut ue::UObjectBase, *mut ue::kismet::FFrame, *mut c_void);
}

unsafe fn patch(bin_dir: PathBuf) -> Result<()> {
    let exe = patternsleuth::process::internal::read_image()?;

    info!("starting scan");
    let resolution = exe.resolve(DllHookResolution::resolver())?;
    info!("finished scan");

    info!("results: {:?}", resolution);

    ue::GMALLOC.set(resolution.gmalloc.0 as *const c_void);
    *ue::FFRAME_STEP.lock().unwrap() = Some(std::mem::transmute(resolution.fframe_step.0));
    *ue::FFRAME_STEP_EXPLICIT_PROPERTY.lock().unwrap() = Some(std::mem::transmute(
        resolution.fframe_step_explicit_property.0,
    ));

    HookUGameEngineTick.initialize(
        std::mem::transmute(resolution.game_tick.0),
        move |game_engine, delta_seconds, idle_mode| {
            //info!("tick time={:0.5}", delta_seconds);
            HookUGameEngineTick.call(game_engine, delta_seconds, idle_mode);
        },
    )?;
    HookUGameEngineTick.enable()?;

    HookAllocateUObject.initialize(
        std::mem::transmute(resolution.allocate_uobject.0),
        |this, object, merging_threads| {
            HookAllocateUObject.call(this, object, merging_threads);
            //info!("allocated object {this:?} {object:?} {merging_threads}");
        },
    )?;
    HookAllocateUObject.enable()?;
    HookFreeUObject.initialize(
        std::mem::transmute(resolution.free_uobject.0),
        |this, object| {
            //info!("freeing object {this:?} {object:?}");
            HookFreeUObject.call(this, object);
        },
    )?;
    HookFreeUObject.enable()?;

    HookKismetPrintString.initialize(
        std::mem::transmute(
            *resolution
                .kismet_system_library
                .0
                .get("PrintString")
                .unwrap(),
        ),
        |context, stack, result| {
            let stack = &mut *stack;

            let mut ctx: Option<&ue::UObject> = None;
            let mut string = ue::FString::default();
            let mut print_to_screen = false;
            let mut print_to_log = false;
            let mut color = ue::FLinearColor::default();
            let mut duration = 0f32;

            ue::kismet::arg(stack, &mut ctx);
            ue::kismet::arg(stack, &mut string);
            ue::kismet::arg(stack, &mut print_to_screen);
            ue::kismet::arg(stack, &mut print_to_log);
            ue::kismet::arg(stack, &mut color);
            ue::kismet::arg(stack, &mut duration);

            let s = string.to_os_string();
            info!("PrintString({s:?})");

            if !stack.code.is_null() {
                stack.code = stack.code.add(1);
            }
        },
    )?;
    HookKismetPrintString.enable()?;

    info!("hooked");

    return Ok(());

    if true {
        std::thread::spawn(move || {
            let guobjectarray = &*(resolution.guobject_array.0 as *const ue::FUObjectArray);
            type FnFNameToString = unsafe extern "system" fn(&ue::FName, &mut ue::FString);

            let fnametostring: FnFNameToString = std::mem::transmute(resolution.fnametostring.0);

            loop {
                info!("a");
                let refs = guobjectarray
                    .iter()
                    .filter(|obj| {
                        if let Some(obj) = obj {
                            let mut name = ue::FString::default();
                            fnametostring(&obj.NamePrivate, &mut name);
                            name.to_os_string()
                                .to_string_lossy()
                                .to_ascii_lowercase()
                                .contains(&"get")
                        } else {
                            false
                        }
                    })
                    .collect::<Vec<_>>();
                for (i, obj) in refs.iter().enumerate() {
                    if let Some(obj) = obj {
                        let mut name = ue::FString::default();
                        fnametostring(&obj.NamePrivate, &mut name);

                        let mut class = ue::FString::default();
                        fnametostring(
                            &(&*obj.ClassPrivate)
                                .UStruct
                                .UField
                                .UObject
                                .UObjectBaseUtility
                                .UObjectBase
                                .NamePrivate,
                            &mut class,
                        );
                        let class_os = class.to_os_string();
                        let class = class_os.to_string_lossy();

                        if class == "Function" {
                            // TODO safe casting
                            let s = &*((*obj as *const _) as *const ue::UStruct);
                            if s.Script.num > 0 {
                                info!("{:x?}", s.Script);
                                info!("{i:10} {} {}", class, name.to_os_string().to_string_lossy(),);
                            }
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1000));
            }
        });
    } else {
        std::thread::spawn(move || {
            gui::main(resolution).unwrap();
        });
    }

    Ok(())
}

mod gui {

    use eframe::egui;

    use egui_winit::winit::platform::windows::EventLoopBuilderExtWindows;

    use super::*;

    pub fn main(resolution: DllHookResolution) -> Result<(), eframe::Error> {
        let event_loop_builder: Option<eframe::EventLoopBuilderHook> =
            Some(Box::new(|event_loop_builder| {
                event_loop_builder.with_any_thread(true);
            }));
        let options = eframe::NativeOptions {
            event_loop_builder,
            viewport: egui::ViewportBuilder::default().with_inner_size([320.0, 240.0]),
            ..Default::default()
        };
        eframe::run_native(
            "My egui App",
            options,
            Box::new(|cc| Box::new(MyApp::new(resolution))),
        )
    }

    struct MyApp {
        resolution: DllHookResolution,
        search: String,
    }

    impl MyApp {
        fn new(resolution: DllHookResolution) -> Self {
            Self {
                resolution,
                search: "".to_owned(),
            }
        }
    }

    impl eframe::App for MyApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.set_visuals(egui::Visuals::dark());

            unsafe {
                let guobjectarray =
                    &*(self.resolution.guobject_array.0 as *const ue::FUObjectArray);
                type FnFNameToString = unsafe extern "system" fn(&ue::FName, &mut ue::FString);

                let fnametostring: FnFNameToString =
                    std::mem::transmute(self.resolution.fnametostring.0);

                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.heading("My egui Application");

                    ui.horizontal(|ui| {
                        if ui.button("start record").clicked() {
                            /*
                            TArray<FString> Options;
                            Options.Add("ReplayStreamerOverride=InMemoryNetworkReplayStreaming");
                            StartRecordingReplay(NewName, NewName, Options);
                            */

                            let start_recording_replay =
                                self.resolution.start_recording_replay.get();

                            //std::thread::spawn(move || {
                            let game_instance = guobjectarray
                                .iter()
                                .find_map(|obj| {
                                    if let Some(obj) = obj.filter(|obj| {
                                        obj.ObjectFlags.contains(ue::EObjectFlags::RF_Transient)
                                    }) {
                                        let mut name = ue::FString::default();
                                        fnametostring(&obj.NamePrivate, &mut name);
                                        name.to_os_string()
                                            .to_string_lossy()
                                            .contains("BP_GameInstance")
                                            .then_some(obj)
                                    } else {
                                        None
                                    }
                                })
                                .unwrap();
                            info!("game_instance = {:?}", game_instance);

                            let name = "test-demo".encode_utf16().collect::<Vec<u16>>();
                            let fstr_name = ue::FString {
                                data: name.as_ptr(),
                                num: name.len() as i32,
                                max: name.len() as i32,
                            };

                            let option1 = "ReplayStreamerOverride=LocalFileNetworkReplayStreaming"
                                .encode_utf16()
                                .collect::<Vec<u16>>();
                            let fstr_option1 = ue::FString {
                                data: option1.as_ptr(),
                                num: option1.len() as i32,
                                max: option1.len() as i32,
                            };

                            //let options = [fstr_option1];
                            let options = [fstr_option1];
                            // TODO BAD cause it gets mutated by UE
                            let tarray_options = ue::TArray::<ue::FString> {
                                data: options.as_ptr(),
                                num: options.len() as i32,
                                max: options.len() as i32,
                            };

                            let tarray_options = ue::TArray::<ue::FString> {
                                data: std::ptr::null(),
                                num: 0,
                                max: 0,
                            };

                            let reference_controller = ue::FReferenceControllerBase {
                                shared_reference_count: 0,
                                weak_reference_count: 0,
                            };

                            info!("calling record {:?}", start_recording_replay);
                            start_recording_replay(
                                game_instance as *const ue::UObjectBase as *const ue::UObject,
                                &fstr_name,
                                &fstr_name,
                                &tarray_options,
                                ue::TSharedPtr {
                                    object: std::ptr::null(),
                                    reference_controller: &reference_controller,
                                },
                            );
                            info!("done calling record");
                            //});
                        }

                        if ui.button("stop record").clicked() {
                            let stop_recording_replay = self.resolution.stop_recording_replay.get();

                            //std::thread::spawn(move || {
                            let game_instance = guobjectarray
                                .iter()
                                .find_map(|obj| {
                                    if let Some(obj) = obj.filter(|obj| {
                                        obj.ObjectFlags.contains(ue::EObjectFlags::RF_Transient)
                                    }) {
                                        let mut name = ue::FString::default();
                                        fnametostring(&obj.NamePrivate, &mut name);
                                        name.to_os_string()
                                            .to_string_lossy()
                                            .contains("BP_GameInstance")
                                            .then_some(obj)
                                    } else {
                                        None
                                    }
                                })
                                .unwrap();
                            info!("game_instance = {:?}", game_instance);

                            info!("calling stop {:?}", stop_recording_replay);
                            stop_recording_replay(
                                game_instance as *const ue::UObjectBase as *const ue::UObject,
                            );
                            info!("done calling stop");
                            //});
                        }
                    });

                    ui.horizontal(|ui| {
                        let name_label = ui.label("Search: ");
                        ui.text_edit_singleline(&mut self.search)
                            .labelled_by(name_label.id);
                    });

                    let text_style = egui::TextStyle::Body;
                    let row_height = ui.text_style_height(&text_style);

                    let (total_rows, refs) = if self.search.is_empty() {
                        (guobjectarray.ObjObjects.NumElements as usize, None)
                    } else {
                        let search = self.search.to_ascii_lowercase();
                        let refs = guobjectarray
                            .iter()
                            .filter(|obj| {
                                if let Some(obj) = obj {
                                    let mut name = ue::FString::default();
                                    fnametostring(&obj.NamePrivate, &mut name);
                                    name.to_os_string()
                                        .to_string_lossy()
                                        .to_ascii_lowercase()
                                        .contains(&search)
                                } else {
                                    false
                                }
                            })
                            .collect::<Vec<_>>();
                        (refs.len(), Some(refs))
                    };

                    egui::ScrollArea::vertical().show_rows(
                        ui,
                        row_height,
                        total_rows,
                        |ui, row_range| {
                            let iter = if let Some(refs) = &refs {
                                itertools::Either::Left(refs.iter().map(|o| *o))
                            } else {
                                itertools::Either::Right(guobjectarray.iter())
                            };
                            for (i, obj) in
                                iter.enumerate().skip(row_range.start).take(row_range.len())
                            {
                                if let Some(obj) = obj {
                                    let mut name = ue::FString::default();
                                    fnametostring(&obj.NamePrivate, &mut name);
                                    ui.label(format!(
                                        "{i:10} {:?} {}",
                                        obj.ObjectFlags,
                                        name.to_os_string().to_string_lossy()
                                    ));
                                } else {
                                    ui.label(format!("{i:10} null"));
                                }
                            }
                            ui.allocate_space(ui.available_size());
                        },
                    );
                });
            }
        }
    }
}

#[allow(non_snake_case)]
mod ue {
    use std::{
        ffi::{c_void, OsString},
        sync::Mutex,
    };

    pub static GMALLOC: GMalloc = GMalloc {
        ptr: Mutex::new(None),
    };

    pub static FFRAME_STEP_EXPLICIT_PROPERTY: Mutex<Option<FnFFrame_StepExplicitProperty>> =
        Mutex::new(None);
    pub static FFRAME_STEP: Mutex<Option<FnFFrame_Step>> = Mutex::new(None);

    pub type FnFFrame_Step =
        unsafe extern "system" fn(stack: &mut kismet::FFrame, *mut UObject, result: *mut c_void);
    pub type FnFFrame_StepExplicitProperty = unsafe extern "system" fn(
        stack: &mut kismet::FFrame,
        result: *mut c_void,
        property: *const FProperty,
    );

    #[derive(Debug)]
    #[repr(C)]
    pub struct GMalloc {
        ptr: Mutex<Option<*const *const FMalloc>>,
    }
    unsafe impl Sync for GMalloc {}
    unsafe impl Send for GMalloc {}
    impl GMalloc {
        pub fn set(&self, gmalloc: *const c_void) {
            *self.ptr.lock().unwrap() = Some(gmalloc as *const *const FMalloc);
        }
        pub fn get(&self) -> &FMalloc {
            unsafe { &**self.ptr.lock().unwrap().unwrap() }
        }
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct FMalloc {
        vtable: *const FMallocVTable,
    }
    unsafe impl Sync for FMalloc {}
    unsafe impl Send for FMalloc {}
    impl FMalloc {
        pub fn malloc(&self, count: usize, alignment: u32) -> *mut c_void {
            unsafe { ((*self.vtable).Malloc)(self, count, alignment) }
        }
        pub fn realloc(&self, original: *mut c_void, count: usize, alignment: u32) -> *mut c_void {
            unsafe { ((*self.vtable).Realloc)(self, original, count, alignment) }
        }
        pub fn free(&self, original: *mut c_void) {
            unsafe { ((*self.vtable).Free)(self, original) }
        }
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct FMallocVTable {
        pub __vecDelDtor: unsafe extern "system" fn(), // TOOD
        pub Exec: unsafe extern "system" fn(),         // TOOD
        pub Malloc:
            unsafe extern "system" fn(this: &FMalloc, count: usize, alignment: u32) -> *mut c_void,
        pub TryMalloc:
            unsafe extern "system" fn(this: &FMalloc, count: usize, alignment: u32) -> *mut c_void,
        pub Realloc: unsafe extern "system" fn(
            this: &FMalloc,
            original: *mut c_void,
            count: usize,
            alignment: u32,
        ) -> *mut c_void,
        pub TryRealloc: unsafe extern "system" fn(
            this: &FMalloc,
            original: *mut c_void,
            count: usize,
            alignment: u32,
        ) -> *mut c_void,
        pub Free: unsafe extern "system" fn(this: &FMalloc, original: *mut c_void),
        pub QuantizeSize: unsafe extern "system" fn(), // TOOD
        pub GetAllocationSize: unsafe extern "system" fn(), // TOOD
        pub Trim: unsafe extern "system" fn(),         // TOOD
        pub SetupTLSCachesOnCurrentThread: unsafe extern "system" fn(), // TOOD
        pub ClearAndDisableTLSCachesOnCurrentThread: unsafe extern "system" fn(), // TOOD // TOOD
        pub InitializeStatsMetadata: unsafe extern "system" fn(), // TOOD
        pub UpdateStats: unsafe extern "system" fn(),  // TOOD
        pub GetAllocatorStats: unsafe extern "system" fn(), // TOOD
        pub DumpAllocatorStats: unsafe extern "system" fn(), // TOOD
        pub IsInternallyThreadSafe: unsafe extern "system" fn(), // TOOD
        pub ValidateHeap: unsafe extern "system" fn(), // TOOD
        pub GetDescriptiveName: unsafe extern "system" fn(), // TOOD
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct FUObjectArray {
        /* offset 0x0000 */ pub ObjFirstGCIndex: i32,
        /* offset 0x0004 */ pub ObjLastNonGCIndex: i32,
        /* offset 0x0008 */ pub MaxObjectsNotConsideredByGC: i32,
        /* offset 0x000c */ pub OpenForDisregardForGC: bool,
        /* offset 0x0010 */
        pub ObjObjects: FChunkedFixedUObjectArray,
        /* offset 0x0030 */ //FWindowsCriticalSection ObjObjectsCritical;
        /* offset 0x0058 */ //TArray<int,TSizedDefaultAllocator<32> > ObjAvailableList;
        /* offset 0x0068 */ //TArray<FUObjectArray::FUObjectCreateListener *,TSizedDefaultAllocator<32> > UObjectCreateListeners;
        /* offset 0x0078 */ //TArray<FUObjectArray::FUObjectDeleteListener *,TSizedDefaultAllocator<32> > UObjectDeleteListeners;
        /* offset 0x0088 */ //FWindowsCriticalSection UObjectDeleteListenersCritical;
        /* offset 0x00b0 */ //FThreadSafeCounter MasterSerialNumber;
    }
    unsafe impl Send for FUObjectArray {}
    unsafe impl Sync for FUObjectArray {}
    impl FUObjectArray {
        pub fn iter(&self) -> ObjectIterator<'_> {
            ObjectIterator {
                array: self,
                index: 0,
            }
        }
    }

    pub struct ObjectIterator<'a> {
        array: &'a FUObjectArray,
        index: i32,
    }
    impl<'a> Iterator for ObjectIterator<'a> {
        type Item = Option<&'a UObjectBase>;
        fn size_hint(&self) -> (usize, Option<usize>) {
            let size = self.array.ObjObjects.NumElements as usize;
            (size, Some(size))
        }
        fn nth(&mut self, n: usize) -> Option<Self::Item> {
            let n = n as i32;
            if self.index < n {
                self.index = n;
            }
            self.next()
        }
        fn next(&mut self) -> Option<Option<&'a UObjectBase>> {
            if self.index >= self.array.ObjObjects.NumElements {
                None
            } else {
                let per_chunk = self.array.ObjObjects.MaxElements / self.array.ObjObjects.MaxChunks;

                let obj = unsafe {
                    let chunk = *self
                        .array
                        .ObjObjects
                        .Objects
                        .add((self.index / per_chunk) as usize);
                    let item = &*chunk.add((self.index % per_chunk) as usize);
                    item.Object.as_ref()
                };

                self.index += 1;
                Some(obj)
            }
        }
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct FChunkedFixedUObjectArray {
        /* offset 0x0000 */ pub Objects: *const *const FUObjectItem,
        /* offset 0x0008 */ pub PreAllocatedObjects: *const FUObjectItem,
        /* offset 0x0010 */ pub MaxElements: i32,
        /* offset 0x0014 */ pub NumElements: i32,
        /* offset 0x0018 */ pub MaxChunks: i32,
        /* offset 0x001c */ pub NumChunks: i32,
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct FUObjectItem {
        /* offset 0x0000 */ pub Object: *const UObjectBase,
        /* offset 0x0008 */ pub Flags: i32,
        /* offset 0x000c */ pub ClusterRootIndex: i32,
        /* offset 0x0010 */ pub SerialNumber: i32,
    }

    bitflags::bitflags! {
        #[derive(Debug, Clone)]
        pub struct EObjectFlags: u32 {
            const RF_NoFlags = 0x0000;
            const RF_Public = 0x0001;
            const RF_Standalone = 0x0002;
            const RF_MarkAsNative = 0x0004;
            const RF_Transactional = 0x0008;
            const RF_ClassDefaultObject = 0x0010;
            const RF_ArchetypeObject = 0x0020;
            const RF_Transient = 0x0040;
            const RF_MarkAsRootSet = 0x0080;
            const RF_TagGarbageTemp = 0x0100;
            const RF_NeedInitialization = 0x0200;
            const RF_NeedLoad = 0x0400;
            const RF_KeepForCooker = 0x0800;
            const RF_NeedPostLoad = 0x1000;
            const RF_NeedPostLoadSubobjects = 0x2000;
            const RF_NewerVersionExists = 0x4000;
            const RF_BeginDestroyed = 0x8000;
            const RF_FinishDestroyed = 0x00010000;
            const RF_BeingRegenerated = 0x00020000;
            const RF_DefaultSubObject = 0x00040000;
            const RF_WasLoaded = 0x00080000;
            const RF_TextExportTransient = 0x00100000;
            const RF_LoadCompleted = 0x00200000;
            const RF_InheritableComponentTemplate = 0x00400000;
            const RF_DuplicateTransient = 0x00800000;
            const RF_StrongRefOnFrame = 0x01000000;
            const RF_NonPIEDuplicateTransient = 0x02000000;
            const RF_Dynamic = 0x04000000;
            const RF_WillBeLoaded = 0x08000000;
        }
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct UObjectBase {
        pub vftable: *const std::ffi::c_void,
        /* offset 0x0008 */ pub ObjectFlags: EObjectFlags,
        /* offset 0x000c */ pub InternalIndex: i32,
        /* offset 0x0010 */ pub ClassPrivate: *const UClass,
        /* offset 0x0018 */ pub NamePrivate: FName,
        /* offset 0x0020 */ pub OuterPrivate: *const UObject,
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct UObjectBaseUtility {
        pub UObjectBase: UObjectBase,
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct UObject {
        pub UObjectBaseUtility: UObjectBaseUtility,
    }

    #[derive(Debug)]
    #[repr(C)]
    struct FOutputDevice {
        vtable: *const c_void,
        /* offset 0x0008 */ bSuppressEventTag: bool,
        /* offset 0x0009 */ bAutoEmitLineTerminator: bool,
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct UField {
        pub UObject: UObject,
        pub Next: *const UField,
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct FStructBaseChain {
        /* offset 0x0000 */ pub StructBaseChainArray: *const *const FStructBaseChain,
        /* offset 0x0008 */ pub NumStructBasesInChainMinusOne: i32,
    }

    #[derive(Debug)]
    #[repr(C)]
    struct FFieldClass {
        // TODO
        /* offset 0x0000 */
        Name: FName,
        /* offset 0x0008 */ //unhandled_primitive.kind /* UQuad */ Id;
        /* offset 0x0010 */ //unhandled_primitive.kind /* UQuad */ CastFlags;
        /* offset 0x0018 */ //EClassFlags ClassFlags;
        /* offset 0x0020 */ //FFieldClass* SuperClass;
        /* offset 0x0028 */ //FField* DefaultObject;
        /* offset 0x0030 */ //Type0x1159e /* TODO: figure out how to name it */* ConstructFn;
        /* offset 0x0038 */ //FThreadSafeCounter UnqiueNameIndexCounter;
    }

    #[derive(Debug)]
    #[repr(C)]
    struct FFieldVariant {
        /* offset 0x0000 */ container: *const c_void,
        /* offset 0x0008 */ bIsUObject: bool,
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct FField {
        /* offset 0x0008 */ ClassPrivate: *const FFieldClass,
        /* offset 0x0010 */ Owner: FFieldVariant,
        /* offset 0x0020 */ Next: *const FField,
        /* offset 0x0028 */ NamePrivate: FName,
        /* offset 0x0030 */ FlagsPrivate: EObjectFlags,
    }

    pub struct FProperty {
        // TODO
        /* offset 0x0000 */ //pub FField: FField,
        /* offset 0x0038 */ //pub ArrayDim: i32,
        /* offset 0x003c */ //pub ElementSize: i32,
        /* offset 0x0040 */ //EPropertyFlags PropertyFlags;
        /* offset 0x0048 */ //unhandled_primitive.kind /* UShort */ RepIndex;
        /* offset 0x004a */ //TEnumAsByte<enum ELifetimeCondition> BlueprintReplicationCondition;
        /* offset 0x004c */ //int32_t Offset_Internal;
        /* offset 0x0050 */ //FName RepNotifyFunc;
        /* offset 0x0058 */ //FProperty* PropertyLinkNext;
        /* offset 0x0060 */ //FProperty* NextRef;
        /* offset 0x0068 */ //FProperty* DestructorLinkNext;
        /* offset 0x0070 */ //FProperty* PostConstructLinkNext;
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct UStruct {
        /* offset 0x0000 */ pub UField: UField,
        /* offset 0x0030 */ pub FStructBaseChain: FStructBaseChain,
        /* offset 0x0040 */ pub SuperStruct: *const UStruct,
        /* offset 0x0048 */ pub Children: *const UField,
        /* offset 0x0050 */ pub ChildProperties: *const FField,
        /* offset 0x0058 */ pub PropertiesSize: i32,
        /* offset 0x005c */ pub MinAlignment: i32,
        /* offset 0x0060 */ pub Script: TArray<u8>,
        /* offset 0x0070 */ pub PropertyLink: *const FProperty,
        /* offset 0x0078 */ pub RefLink: *const FProperty,
        /* offset 0x0080 */ pub DestructorLink: *const FProperty,
        /* offset 0x0088 */ pub PostConstructLink: *const FProperty,
        /* offset 0x0090 */
        pub ScriptAndPropertyObjectReferences: TArray<*const UObject>,
        /* offset 0x00a0 */
        pub UnresolvedScriptProperties: *const (), //TODO pub TArray<TTuple<TFieldPath<FField>,int>,TSizedDefaultAllocator<32> >*
        /* offset 0x00a8 */
        pub UnversionedSchema: *const (), //TODO const FUnversionedStructSchema*
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct UClass {
        /* offset 0x0000 */ pub UStruct: UStruct,
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct FName {
        /* offset 0x0000 */ pub ComparisonIndex: FNameEntryId,
        /* offset 0x0004 */ pub Number: u32,
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct FNameEntryId {
        /* offset 0x0000 */ pub Value: u32,
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct TSharedPtr<T> {
        pub object: *const T,
        pub reference_controller: *const FReferenceControllerBase,
    }

    #[derive(Debug)]
    #[repr(C)]
    pub struct FReferenceControllerBase {
        pub shared_reference_count: i32,
        pub weak_reference_count: i32,
    }

    pub type FString = TArray<u16>;

    #[derive(Debug)]
    #[repr(C)]
    pub struct TArray<T> {
        pub data: *const T,
        pub num: i32,
        pub max: i32,
    }
    impl<T> Drop for TArray<T> {
        fn drop(&mut self) {
            GMALLOC.get().free(self.data as *mut c_void);
        }
    }
    impl<T> Default for TArray<T> {
        fn default() -> Self {
            Self {
                data: std::ptr::null(),
                num: 0,
                max: 0,
            }
        }
    }
    impl<T> TArray<T> {
        pub fn as_slice(&self) -> &[T] {
            if self.num == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(self.data, self.num as usize) }
            }
        }
        pub fn as_slice_mut(&mut self) -> &mut [T] {
            if self.num == 0 {
                &mut []
            } else {
                unsafe { std::slice::from_raw_parts_mut(self.data as *mut _, self.num as usize) }
            }
        }
        pub fn from_slice(slice: &[T]) -> TArray<T> {
            TArray {
                data: slice.as_ptr(),
                num: slice.len() as i32,
                max: slice.len() as i32,
            }
        }
    }

    impl FString {
        pub fn to_os_string(&self) -> OsString {
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::ffi::OsStringExt;
                let slice = self.as_slice();
                let len = slice
                    .iter()
                    .enumerate()
                    .find_map(|(i, &b)| (b == 0).then_some(i))
                    .unwrap_or(slice.len());
                std::ffi::OsString::from_wide(&slice[0..len])
            }
            #[cfg(not(target_os = "windows"))]
            unimplemented!()
        }
    }

    #[derive(Debug, Default)]
    #[repr(C)]
    pub struct FVector {
        pub x: f32,
        pub y: f32,
        pub z: f32,
    }

    #[derive(Debug, Default)]
    #[repr(C)]
    pub struct FLinearColor {
        pub r: f32,
        pub g: f32,
        pub b: f32,
        pub a: f32,
    }

    pub mod kismet {
        use super::*;

        #[derive(Debug)]
        #[repr(C)]
        pub struct FFrame {
            /* offset 0x0000 */ pub base: FOutputDevice,
            /* offset 0x0010 */ pub node: *const c_void,
            /* offset 0x0018 */ pub object: *mut UObject,
            /* offset 0x0020 */ pub code: *const c_void,
            /* offset 0x0028 */ pub locals: *const c_void,
            /* offset 0x0030 */ pub most_recent_property: *const FProperty,
            /* offset 0x0038 */ pub most_recent_property_address: *const c_void,
            /* offset 0x0040 */ pub flow_stack: [u8; 0x30],
            /* offset 0x0070 */ pub previous_frame: *const FFrame,
            /* offset 0x0078 */ pub out_parms: *const c_void,
            /* offset 0x0080 */ pub property_chain_for_compiled_in: *const FField,
            /* offset 0x0088 */ pub current_native_function: *const c_void,
            /* offset 0x0090 */ pub b_array_context_failed: bool,
        }

        pub fn arg<T: Sized>(stack: &mut FFrame, output: &mut T) {
            //dbg!(&stack);
            let output = output as *const _ as *mut _;
            unsafe {
                //simple_log::info!("{:x?}", stack);
                if stack.code.is_null() {
                    let cur = stack.property_chain_for_compiled_in;
                    stack.property_chain_for_compiled_in = (*cur).Next;
                    FFRAME_STEP_EXPLICIT_PROPERTY.lock().unwrap().unwrap()(
                        stack,
                        output,
                        cur as *const FProperty,
                    );
                } else {
                    FFRAME_STEP.lock().unwrap().unwrap()(stack, stack.object, output);
                }
            }
        }
    }
}
