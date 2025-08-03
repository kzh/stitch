fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::compile_protos("proto/stitch/v1/service.proto")?;
    Ok(())
}
