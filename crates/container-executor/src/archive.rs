use crate::*;
pub(crate) fn safe_name(name: &str) -> ExecutorResult {
    if name.is_empty()
        || split_ustar_name(name).is_err()
        || name.contains(['\0', '\\', ':'])
        || name.starts_with('/')
        || name
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || name.split('/').count() > 64
    {
        return Err(ExecutorError::Invalid(
            "unsafe or unsupported snapshot path".into(),
        ));
    }
    Ok(())
}
fn split_ustar_name(name: &str) -> ExecutorResult<(&str, &str)> {
    if name.len() <= 100 {
        return Ok(("", name));
    }
    for (index, _) in name.rmatch_indices('/') {
        if index <= 155 && name.len() - index - 1 <= 100 {
            return Ok((&name[..index], &name[index + 1..]));
        }
    }
    Err(ExecutorError::Invalid(
        "snapshot path cannot fit ustar prefix/name fields".into(),
    ))
}
fn octal(dest: &mut [u8], value: u64) {
    let value = format!("{value:0width$o}", width = dest.len() - 1);
    dest[..value.len()].copy_from_slice(value.as_bytes());
}
pub(crate) fn pack(snapshot: &Snapshot) -> ExecutorResult<Vec<u8>> {
    snapshot.validate(MAX_CAPTURE)?;
    let size = 1024
        + snapshot
            .files
            .values()
            .map(|data| 512 + data.len().div_ceil(512) * 512)
            .sum::<usize>();
    let mut tar = Vec::with_capacity(size);
    for (name, data) in &snapshot.files {
        let mut header = [0u8; 512];
        let (prefix, leaf) = split_ustar_name(name)?;
        header[..leaf.len()].copy_from_slice(leaf.as_bytes());
        header[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        octal(&mut header[100..108], 0o644);
        octal(&mut header[108..116], 65534);
        octal(&mut header[116..124], 65534);
        octal(&mut header[124..136], data.len() as u64);
        octal(&mut header[136..148], 0);
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let sum = header.iter().map(|b| u64::from(*b)).sum();
        octal(&mut header[148..155], sum);
        header[155] = b' ';
        tar.extend(header);
        tar.extend(data);
        tar.resize(tar.len().div_ceil(512) * 512, 0);
    }
    tar.resize(tar.len() + 1024, 0);
    Ok(tar)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_utf8_names_use_ustar_prefix_without_extensions_or_traversal() {
        let prefix = "ü".repeat(70);
        let leaf = "x".repeat(99);
        let name = format!("{prefix}/{leaf}");
        let bytes = pack(&Snapshot {
            files: BTreeMap::from([(name.clone(), b"body".to_vec())]),
        })
        .unwrap();
        assert_eq!(&bytes[..99], leaf.as_bytes());
        assert_eq!(&bytes[345..485], prefix.as_bytes());
        assert_eq!(&bytes[512..516], b"body");
        for bad in [
            "../escape".to_owned(),
            "a/../escape".into(),
            "a".repeat(101),
            format!("{}/x", "b".repeat(156)),
        ] {
            assert!(safe_name(&bad).is_err(), "{bad}");
        }
    }
}
