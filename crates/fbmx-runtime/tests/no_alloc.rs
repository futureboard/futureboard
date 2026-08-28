//! Proof that the realtime path does not allocate.
//!
//! "No allocation in `process_block`" is a claim that rots the moment somebody
//! adds a `Vec` for convenience, and a code review will not reliably catch it.
//! A counting global allocator will.
//!
//! The same argument covers the other realtime prohibitions: nothing in
//! `process_block` can touch the filesystem or parse JSON without allocating,
//! so a zero-allocation block is strong evidence for all of them. There is no
//! `Mutex`, `RwLock` or logging anywhere in the crate — `grep` is the check
//! for those, and `deny(clippy::print_stdout)`-style lints are left to CI.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

mod support;

use fbmx_runtime::{AudioModel, FbmxModel};
use support::*;

struct CountingAllocator;

// Per thread, not global: cargo runs tests concurrently in one process, and a
// global counter would attribute another test's allocations to this one.
// `const`-initialised Cells have no destructor, so touching them from inside
// the allocator cannot itself allocate.
thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

fn note_allocation() {
    // `try_with`: during thread teardown the TLS is gone, and panicking inside
    // the allocator would abort the process.
    let _ = COUNTING.try_with(|counting| {
        if counting.get() {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        }
    });
}

#[allow(unsafe_code)]
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Run `f` with allocation counting enabled and report how many happened.
fn count_allocations(f: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|n| n.set(0));
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    ALLOCATIONS.with(|n| n.get())
}

#[test]
fn processing_allocates_nothing() {
    let model = FbmxModel::load(golden_model("smoke_lstm32")).expect("golden model");
    let mut engine = model.instantiate().expect("instantiate");
    engine.set_parameter("drive", 0.5).unwrap();
    engine.refresh_conditioning();

    // Buffers belong to the host; they are allocated before the callback.
    let input = vec![0.1f32; 1024];
    let mut output = vec![0.0f32; 1024];
    let mut in_place = vec![0.2f32; 1024];

    // Warm up outside the counted region so nothing lazy is left.
    engine.process_block(&input[..16], &mut output[..16]);

    let allocations = count_allocations(|| {
        for size in [16usize, 32, 64, 128, 256, 512, 1024] {
            engine.process_block(&input[..size], &mut output[..size]);
            engine.process_block_in_place(&mut in_place[..size]);
        }
        for i in 0..256 {
            let _ = engine.process_sample(input[i]);
        }
        engine.reset();
        // Parameter changes are realtime too: a host automates these.
        engine.set_parameter_at(0, 0.75);
        engine.set_category_at(0, 1);
        engine.refresh_conditioning();
        engine.process_block(&input, &mut output);
    });

    assert_eq!(
        allocations, 0,
        "the realtime path allocated {allocations} times; \
         something in process_block/reset/set_parameter needs a preallocated buffer"
    );
}

/// Idle compensation runs a second copy of the model on the audio thread, and
/// the `Option::take`/put dance in `idle_offset` moves an `IdleTwin` — three
/// vector headers — on every unsettled sample. Moving a `Vec` must not clone
/// it, and this is where that assumption gets checked rather than assumed.
#[test]
fn idle_compensation_allocates_nothing_either() {
    let model = FbmxModel::load(golden_model("smoke_lstm32")).expect("golden model");
    let mut engine = model.instantiate().expect("instantiate");
    engine.set_idle_compensation(true); // allocates here, before the audio thread
    engine.refresh_conditioning();

    let input = vec![0.1f32; 1024];
    let mut output = vec![0.0f32; 1024];
    engine.process_block(&input[..16], &mut output[..16]);

    let allocations = count_allocations(|| {
        // While the twin is still settling, which is the expensive path.
        engine.process_block(&input, &mut output);
        // And across the parameter changes that unsettle it again.
        for step in 0..8 {
            engine.set_parameter_at(0, step as f32 / 8.0);
            engine.refresh_conditioning();
            engine.process_block(&input[..256], &mut output[..256]);
        }
        engine.reset();
        engine.process_block(&input, &mut output);
    });

    assert_eq!(
        allocations, 0,
        "idle compensation allocated {allocations} times on the realtime path"
    );
}

#[test]
fn name_based_parameter_setting_is_also_allocation_free() {
    // Hosts that have not cached indices still call these per block; a
    // `format!` or a `to_string()` on this path would be a dropout.
    let model = FbmxModel::load(golden_model("smoke_lstm32")).unwrap();
    let mut engine = model.instantiate().unwrap();
    engine.set_parameter("drive", 0.1).unwrap();

    let allocations = count_allocations(|| {
        engine.set_parameter("drive", 0.9).unwrap();
        engine.set_category("mode", "hard").unwrap();
        engine.refresh_conditioning();
    });
    assert_eq!(
        allocations, 0,
        "parameter setting allocated {allocations} times"
    );
}

#[test]
fn loading_is_where_the_allocation_happens() {
    // The counterpart claim: preparation is allowed to allocate, and does.
    // If this ever hit zero the test above would be measuring nothing.
    let bytes = std::fs::read(golden_model("smoke_lstm32")).unwrap();
    let allocations = count_allocations(|| {
        let model = FbmxModel::from_bytes(&bytes).unwrap();
        let _engine = model.instantiate().unwrap();
    });
    assert!(allocations > 0);
}
