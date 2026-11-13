//! WASM module that loads an ONNX BERT model and runs embedding inference.
//!
//! Compiled to `wasm32-wasip1` by the build script in `memnoth-embedding`.
//! The host module loads this WASM binary through `wasmtime` and drives
//! inference by writing tokenised tensors into shared linear memory and
//! reading the pooled output back out.
//!
//! The module owns the loaded model and a scratch output buffer. Because
//! wasmtime instances are single-threaded by default (unless the `threads`
//! proposal is enabled), the API is deliberately synchronous: one embed at a
//! time.

use std::cell::RefCell;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};

use tract_hir::internal::{SimplePlan, TractError};
use tract_hir::prelude::*;
use tract_onnx::onnx;

/// The loaded, runnable model: `into_runnable` yields an `Arc`.
type Runnable = std::sync::Arc<SimplePlan<TypedFact, Box<dyn TypedOp>>>;

thread_local! {
    /// The loaded, runnable model, stored here so ownership crosses the
    /// init/run boundary.
    static MODEL: RefCell<Option<Runnable>> = const { RefCell::new(None) };

    /// Output buffer reused across calls to avoid re-allocation inside the loop.
    static OUTPUT: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };

    /// Error message from the last failed call, stored as a byte slice in
    /// linear memory. The host reads it back via the error pointer/len exports.
    static ERROR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Sentinel: 0 = init not yet called, 1 = model loaded.
static INITIALISED: AtomicBool = AtomicBool::new(false);
/// Size of the output buffer (number of f32 elements).
static OUTPUT_LEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Length of the error message in bytes.
static ERROR_LEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Build a `[1, seq_len]` i64 tensor from a flat slice, mirroring
/// `TokenRows::tensor` in the native backend.
fn make_tensor(data: &[i64], seq_len: usize) -> Result<Tensor, TractError> {
    let arr = tract_ndarray::Array2::from_shape_vec((1, seq_len), data.to_vec())?;
    let tensor: Tensor = arr.into();
    Ok(tensor)
}

/// Load and compile an ONNX model from a buffer of bytes.
///
/// `model_bytes` / `model_bytes_len` point into this module's linear memory.
/// Returns 0 on success, non-zero on failure (check the error exports).
///
/// The model is expected to be an ONNX BERT-style export with the standard
/// three inputs: `input_ids`, `attention_mask`, and `token_type_ids`.
/// Input shapes are concretised to `[1, seq_len]` where `seq_len` is fixed
/// for the lifetime of the instance.
///
/// # Safety
/// `model_bytes` must point to `model_bytes_len` readable bytes in
/// linear memory, or be null only when `model_bytes_len` is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wasm_embed_init(
    model_bytes: *const u8,
    model_bytes_len: usize,
    seq_len: usize,
) -> i32 {
    if model_bytes.is_null() || model_bytes_len == 0 {
        store_error("wasm_embed_init: null or empty model buffer");
        return 1;
    }

    let mut model_slice = unsafe { slice::from_raw_parts(model_bytes, model_bytes_len) };

    let result: Result<Runnable, String> = (|| {
        let mut graph: tract_hir::infer::InferenceModel =
            onnx()
                .model_for_read(&mut model_slice)
                .map_err(|e| format!("model load: {e}"))?;

        // Concretise input shapes to [1, seq_len] for all declared inputs.
        let input_outlets = graph
            .input_outlets()
            .map_err(|e| format!("input outlets: {e}"))?;
        for idx in 0..input_outlets.len() {
            graph = graph
                .with_input_fact(
                    idx,
                    InferenceFact::dt_shape(DatumType::I64, tvec!(1, seq_len)),
                )
                .map_err(|e| format!("input fact: {e}"))?;
        }

        let optimized = graph
            .into_optimized()
            .map_err(|e| format!("optimize: {e}"))?;

        let runnable = optimized
            .into_runnable()
            .map_err(|e| format!("compile: {e}"))?;

        Ok(runnable)
    })();

    match result {
        Ok(runnable) => {
            MODEL.with(|m| *m.borrow_mut() = Some(runnable));
            OUTPUT.with(|o| o.borrow_mut().clear());
            ERROR.with(|e| e.borrow_mut().clear());
            OUTPUT_LEN.store(0, Ordering::SeqCst);
            ERROR_LEN.store(0, Ordering::SeqCst);
            INITIALISED.store(true, Ordering::SeqCst);
            0
        }
        Err(err) => {
            store_error(&format!("wasm_embed_init: {err}"));
            1
        }
    }
}

/// Run one forward pass and write the pooled + normalised output into `OUTPUT`.
///
/// The three input arrays are written as flat `[seq_len]` slices into linear
/// memory before the call. `seq_len` must match the value passed to
/// `wasm_embed_init`.
///
/// Returns 0 on success, non-zero on failure.
///
/// # Safety
/// Each input pointer must point to `seq_len` readable `i64` values in
/// linear memory, or be null (which is rejected before any dereference).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wasm_embed_run(
    input_ids: *const i64,
    attention_mask: *const i64,
    token_type_ids: *const i64,
    seq_len: usize,
) -> i32 {
    if !INITIALISED.load(Ordering::SeqCst) {
        store_error("wasm_embed_run: model not initialised, call wasm_embed_init first");
        return 1;
    }

    if input_ids.is_null() || attention_mask.is_null() || token_type_ids.is_null() {
        store_error("wasm_embed_run: null input pointer");
        return 2;
    }

    let ids = unsafe { slice::from_raw_parts(input_ids, seq_len) };
    let mask = unsafe { slice::from_raw_parts(attention_mask, seq_len) };
    let types = unsafe { slice::from_raw_parts(token_type_ids, seq_len) };

    // Build input tensors using the same pattern as TokenRows::tensor
    // in the native backend: Array2 → Tensor → cast → owned → TValue.
    let ids_tensor = match make_tensor(ids, seq_len) {
        Ok(t) => t,
        Err(e) => {
            store_error(&format!("ids tensor: {e}"));
            return 3;
        }
    };

    let mask_tensor = match make_tensor(mask, seq_len) {
        Ok(t) => t,
        Err(e) => {
            store_error(&format!("mask tensor: {e}"));
            return 4;
        }
    };

    let types_tensor = match make_tensor(types, seq_len) {
        Ok(t) => t,
        Err(e) => {
            store_error(&format!("types tensor: {e}"));
            return 5;
        }
    };

    let inputs: TVec<TValue> = tvec!(
        ids_tensor
            .cast_to_dt(DatumType::I64)
            .unwrap()
            .into_owned()
            .into(),
        mask_tensor
            .cast_to_dt(DatumType::I64)
            .unwrap()
            .into_owned()
            .into(),
        types_tensor
            .cast_to_dt(DatumType::I64)
            .unwrap()
            .into_owned()
            .into(),
    );

    let model = MODEL.with(|m| m.borrow().as_ref().map(|arc| arc.clone()));
    let model = match model {
        Some(m) => m,
        None => {
            store_error("wasm_embed_run: model was unloaded");
            return 6;
        }
    };

    let outputs = match model.run(inputs) {
        Ok(o) => o,
        Err(e) => {
            store_error(&format!("run: {e}"));
            return 7;
        }
    };

    let output = match outputs[0].to_plain_array_view::<f32>() {
        Ok(v) => v.to_owned(),
        Err(e) => {
            store_error(&format!("output view: {e}"));
            return 8;
        }
    };

    let shape = output.shape();
    if shape.len() != 3 {
        store_error(&format!("unexpected output shape: {:?}", shape));
        return 9;
    }

    let hidden = shape[2];
    let rows = shape[1].min(seq_len);

    // Pool: mean over real tokens (where mask == 1).
    let mut pooled = vec![0f32; hidden];
    for (slice, &m) in output
        .axis_iter(tract_ndarray::Axis(1))
        .take(rows)
        .zip(mask.iter())
    {
        if m == 0 {
            continue;
        }
        for (col, value) in pooled.iter_mut().enumerate() {
            *value += slice[[0, col]];
        }
    }

    let divisor = rows.max(1) as f32;
    for v in &mut pooled {
        *v /= divisor;
    }

    // Scale to unit length (matches the native backend's post-processing).
    let magnitude = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
    if magnitude > 0.0 {
        let scale = 1.0 / magnitude;
        for v in &mut pooled {
            *v *= scale;
        }
    }

    OUTPUT.with(|o| {
        let mut out = o.borrow_mut();
        out.clear();
        out.extend_from_slice(&pooled);
    });
    OUTPUT_LEN.store(pooled.len(), Ordering::SeqCst);
    ERROR.with(|e| e.borrow_mut().clear());
    ERROR_LEN.store(0, Ordering::SeqCst);
    0
}

/// Returns a raw pointer to the output float buffer in linear memory.
///
/// # Safety
/// The pointer is valid only within the same thread and between calls
/// to `wasm_embed_run`. The host must read `wasm_embed_output_len` before
/// dereferencing.
#[unsafe(no_mangle)]
pub extern "C" fn wasm_embed_output_ptr() -> *const f32 {
    OUTPUT.with(|o| o.borrow().as_ptr())
}

/// Returns the length of the output float buffer.
#[unsafe(no_mangle)]
pub extern "C" fn wasm_embed_output_len() -> usize {
    OUTPUT_LEN.load(Ordering::SeqCst)
}

/// Returns the pointer to the error message byte buffer.
#[unsafe(no_mangle)]
pub extern "C" fn wasm_embed_error_ptr() -> *const u8 {
    ERROR.with(|e| e.borrow().as_ptr())
}

/// Returns the length of the error message byte buffer.
#[unsafe(no_mangle)]
pub extern "C" fn wasm_embed_error_len() -> usize {
    ERROR_LEN.load(Ordering::SeqCst)
}

/// Stores an error message for later retrieval by the host.
fn store_error(msg: &str) {
    ERROR.with(|e| {
        let mut err = e.borrow_mut();
        err.clear();
        err.extend_from_slice(msg.as_bytes());
    });
    ERROR_LEN.store(msg.len(), Ordering::SeqCst);
}
