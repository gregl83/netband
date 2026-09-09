use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, pem::PemObject};

pub(crate) fn root_store(ca_cert: Option<&std::path::Path>) -> Result<RootCertStore, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = ca_cert {
        let metadata = std::fs::metadata(path)
            .map_err(|error| format!("cannot read private CA {}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("private CA is not a file: {}", path.display()));
        }
        let contents = std::fs::read(path)
            .map_err(|error| format!("cannot read private CA {}: {error}", path.display()))?;
        let mut found = false;
        for certificate in CertificateDer::pem_slice_iter(&contents) {
            let certificate = certificate
                .map_err(|error| format!("invalid private CA {}: {error}", path.display()))?;
            roots
                .add(certificate)
                .map_err(|error| format!("invalid private CA {}: {error}", path.display()))?;
            found = true;
        }
        if !found {
            return Err(format!(
                "private CA {} contains no certificates",
                path.display()
            ));
        }
    }
    Ok(roots)
}
