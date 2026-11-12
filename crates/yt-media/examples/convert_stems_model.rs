#[cfg(all(feature = "metal-stems", target_os = "macos"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use candle_core::{pickle::PthTensors, safetensors, DType};
    use sha2::{Digest, Sha256};
    use std::{collections::HashMap, fs::File, io, path::Path};

    const SOURCE_SHA256: &str = "5b84f37e8d444c8cb30c79d77f613a41c05868ff9c9ac6c7049c00aefae115aa";
    const EXPORT_SHA256: &str = "97307b43fa9a830a80e7d382fab39a746b2a0777ba38cf4d1c7432ef77ad19a2";

    fn sha256(path: &Path) -> io::Result<String> {
        let mut hash = Sha256::new();
        io::copy(&mut File::open(path)?, &mut hash)?;
        Ok(format!("{:x}", hash.finalize()))
    }

    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let [source, output] = args.as_slice() else {
        return Err("Usage: convert_stems_model SOURCE.ckpt OUTPUT.safetensors".into());
    };
    let source = Path::new(source);
    let output = Path::new(output);
    if output.exists() {
        return Err(format!("Output already exists: {}", output.display()).into());
    }
    if sha256(source)? != SOURCE_SHA256 {
        return Err("Source does not match the pinned BS-RoFormer viperx checkpoint".into());
    }

    let checkpoint = PthTensors::new(source, None)?;
    let names = checkpoint.tensor_infos();
    if names.len() != 699 {
        return Err(format!("Expected 699 tensors, found {}", names.len()).into());
    }
    let mut weights = HashMap::with_capacity(names.len());
    for name in names.keys() {
        let tensor = checkpoint.get(name)?.ok_or("Missing checkpoint tensor")?;
        if tensor.dtype() != DType::F32 {
            return Err(format!("Expected float32 tensor: {name}").into());
        }
        weights.insert(name.clone(), tensor.to_dtype(DType::F16)?);
    }

    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    safetensors::save(&weights, temporary.path())?;
    if sha256(temporary.path())? != EXPORT_SHA256 {
        return Err("Converted weights do not match the pinned model export".into());
    }
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(output)?;
    println!("Verified model written to {}", output.display());
    Ok(())
}

#[cfg(not(all(feature = "metal-stems", target_os = "macos")))]
fn main() {
    eprintln!("This converter requires --features metal-stems on macOS.");
    std::process::exit(1);
}
