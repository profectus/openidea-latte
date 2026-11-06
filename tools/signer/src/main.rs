use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageSignature {
    publisher: String,
    key: String,
    signature: String,
}

fn hashed_path(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn package_hash(dir: &Path, files: &[PathBuf]) -> std::io::Result<String> {
    let mut sorted: Vec<&PathBuf> = files.iter().collect();
    sorted.sort();
    sorted.dedup();
    let mut hasher = blake3::Hasher::new();
    for rel in sorted {
        hasher.update(hashed_path(rel).as_bytes());
        hasher.update(&[0]);
        let mut f = File::open(dir.join(rel))?;
        let mut buf = [0u8; 16384];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        hasher.update(&[0]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(".");
    let publisher = "profectus";
    let plugin_id = "profectus.latte";
    let version = "0.1.0";
    let key_file = Path::new("profectus.key");

    // 1. Load or generate signing key
    let signing_key = if key_file.exists() {
        let content = std::fs::read_to_string(key_file)?;
        let bytes = B64.decode(content.trim())?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "Invalid 32-byte key in keyfile")?;
        SigningKey::from_bytes(&seed)
    } else {
        println!("==> Generating new publisher key: {}", key_file.display());
        let key = SigningKey::generate(&mut OsRng);
        std::fs::write(key_file, B64.encode(key.to_bytes()))?;
        key
    };

    let public_key_b64 = B64.encode(signing_key.verifying_key().to_bytes());
    println!("Publisher:   {}", publisher);
    println!("Public key:  {}", public_key_b64);

    // 2. Files to include in package
    let files = vec![
        PathBuf::from("plugin.toml"),
        PathBuf::from("LICENSE"),
        PathBuf::from("README.md"),
        PathBuf::from("icon.svg"),
        PathBuf::from("icon_dark.svg"),
        PathBuf::from("languages/latte.toml"),
        PathBuf::from("icons/latte.svg"),
        PathBuf::from("icons/latte_dark.svg"),
        PathBuf::from("templates/template.latte"),
        PathBuf::from("dist/plugin.wasm"),
    ];

    // 3. Compute blake3 digest
    let digest = package_hash(dir, &files)?;
    println!("Digest:      {}", digest);

    // 4. Compute Ed25519 signature
    let signed_msg = format!("openidea-plugin/1\n{plugin_id}\n{version}\n{digest}");
    let sig = signing_key.sign(signed_msg.as_bytes());
    let sig_b64 = B64.encode(sig.to_bytes());

    let pkg_sig = PackageSignature {
        publisher: publisher.to_string(),
        key: public_key_b64.clone(),
        signature: sig_b64,
    };

    // 5. Pack into ZIP
    let out_name = format!("{plugin_id}-{version}.oiplugin");
    let out_path = Path::new(&out_name);
    let file = File::create(out_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut sorted_files = files.clone();
    sorted_files.sort();
    sorted_files.dedup();

    for rel in sorted_files {
        let name = rel.to_str().unwrap().replace('\\', "/");
        zip.start_file(name, options)?;
        let bytes = std::fs::read(dir.join(&rel))?;
        zip.write_all(&bytes)?;
    }

    // SIGNATURE.json
    zip.start_file("SIGNATURE.json", options)?;
    let sig_json = serde_json::to_string_pretty(&pkg_sig)?;
    zip.write_all(sig_json.as_bytes())?;
    zip.finish()?;

    // 6. Compute SHA-256 of package
    let pkg_bytes = std::fs::read(out_path)?;
    let mut sha256 = Sha256::new();
    sha256.update(&pkg_bytes);
    let pkg_sha256 = format!("{:x}", sha256.finalize());

    println!("Package:     {}", out_name);
    println!("Package size:{} bytes", pkg_bytes.len());
    println!("SHA-256:     {}", pkg_sha256);

    // Write PR helper files in a generated folder
    let pr_dir = Path::new("dist/pr_files");
    std::fs::create_dir_all(pr_dir.join("publishers"))?;
    std::fs::create_dir_all(pr_dir.join("plugins"))?;

    let pub_toml = format!(
        r#"name = "Filip Štencl"
owners = ["{publisher}"]
keys = ["{public_key_b64}"]
revoked-keys = []
"#
    );
    std::fs::write(pr_dir.join(format!("publishers/{publisher}.toml")), pub_toml)?;

    let plugin_toml = format!(
        r#"repository = "https://github.com/{publisher}/openidea-latte"

[[versions]]
version = "{version}"
package = "https://github.com/{publisher}/openidea-latte/releases/download/v{version}/{out_name}"
sha256 = "{pkg_sha256}"
"#
    );
    std::fs::write(
        pr_dir.join(format!("plugins/{plugin_id}.toml")),
        plugin_toml,
    )?;

    println!("\n==> Generated PR files in dist/pr_files/");
    Ok(())
}
