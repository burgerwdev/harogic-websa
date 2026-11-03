//! Inspect and exercise the published streaming ONNX contracts without Python.
use tract_onnx::prelude::*;
use tract_onnx::tract_core::anyhow::{anyhow,ensure};

fn main() -> TractResult<()> {
    let library=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtime/linux-x86_64/libonnxruntime.so.1.23.2");
    ort::init_from(library)?.commit();
    for path in std::env::args().skip(1) {
        println!("MODEL {path}");
        let model = tract_onnx::onnx().model_for_path(&path)?;
        for (i, &id) in model.input_outlets()?.iter().enumerate() {
            println!("  input {i}: {} {:?}", model.node(id.node).name, model.outlet_fact(id)?);
        }
        for (i, &id) in model.output_outlets()?.iter().enumerate() {
            println!("  output {i}: {} {:?}", model.node(id.node).name, model.outlet_fact(id)?);
        }
        let model = model.into_optimized()?;
        let inputs = model.input_outlets()?.iter().map(|id| {
            Tensor::zero::<f32>(&model.outlet_fact(*id)?.shape.as_concrete().unwrap()).map(Into::into)
        }).collect::<TractResult<TVec<TValue>>>()?;
        let shapes:Vec<Vec<usize>>=inputs.iter().map(|v|v.shape().to_vec()).collect();
        let mut native=ort::session::Session::builder()?
            .with_intra_threads(1).map_err(|e|anyhow!(e.to_string()))?
            .with_inter_threads(1).map_err(|e|anyhow!(e.to_string()))?.commit_from_file(&path)?;
        let mut caches=shapes[1..].iter().map(|s|ort::value::Tensor::from_array((s.clone(),vec![0.0f32;s.iter().product()]))).collect::<ort::Result<Vec<_>>>()?;
        let plan = model.into_runnable()?;
        let now = std::time::Instant::now();
        let mut input = inputs;
        let (mut error,mut energy)=(0.0f64,0.0f64);let mut max_error=0.0f32;
        for frame in 0..10 {
            let data:Vec<f32>=(0..shapes[0].iter().product()).map(|i|0.1*((i+frame*13) as f32*0.073).sin()).collect();
            input[0]=Tensor::from_shape(&shapes[0],&data)?.into();
            let tensor=ort::value::Tensor::from_array((shapes[0].clone(),data))?;
            let mut native_inputs:Vec<ort::session::SessionInputValue<'_>>=vec![(&tensor).into()];
            native_inputs.extend(caches.iter().map(Into::into));
            let result=native.run(native_inputs.as_slice())?;
            let out = plan.run(input)?;
            drop(native_inputs);
            for (i,(_,value)) in result.iter().enumerate() {
                let (_,b)=value.try_extract_tensor::<f32>()?;let a=out[i].as_slice::<f32>()?;
                for (&a,&b) in a.iter().zip(b) {let d=(a-b) as f64;error+=d*d;energy+=(b as f64).powi(2);max_error=max_error.max(d.abs() as f32);}
                if i>0 {caches[i-1].try_extract_tensor_mut::<f32>()?.1.copy_from_slice(b);}
            }
            input = tvec!(Tensor::zero::<f32>(&out[0].shape())?.into());
            input.extend(out.into_iter().skip(1));
        }
        let relative_db=10.0*(error/energy.max(1e-30)).log10();
        println!("  10 frames: {:?}; Tract/native-ORT max error {max_error}, relative {relative_db} dB", now.elapsed());
        ensure!(relative_db < -75.0 && max_error<1e-3,"independent native engines disagree");
    }
    Ok(())
}
