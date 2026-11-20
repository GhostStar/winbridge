use anyhow::{ensure, Context, Result};

#[derive(Default)]
pub struct ParameterSets {
    sps: Vec<u8>,
    pps: Vec<u8>,
    length_bytes: Option<usize>,
}

fn start_code(data: &[u8]) -> Option<(usize, usize)> {
    let i = data.windows(3).position(|p| p == [0, 0, 1])?;
    if i > 0 && data[i - 1] == 0 {
        Some((i - 1, 4))
    } else {
        Some((i, 3))
    }
}
fn annex_units(mut bytes: &[u8]) -> Result<Vec<&[u8]>> {
    let mut nals = Vec::new();
    while !bytes.is_empty() {
        let (offset, len) = start_code(bytes).context("missing H.264 start code")?;
        ensure!(
            bytes[..offset].iter().all(|v| *v == 0),
            "invalid H.264 leading bytes"
        );
        bytes = &bytes[offset + len..];
        let end = start_code(bytes).map_or(bytes.len(), |p| p.0);
        ensure!(end > 0, "empty H.264 NAL");
        nals.push(&bytes[..end]);
        bytes = &bytes[end..];
    }
    Ok(nals)
}
fn length_unit<'a>(data: &mut &'a [u8], len_size: usize) -> Result<&'a [u8]> {
    let prefix = data.get(..len_size).context("truncated NAL length")?;
    let len = prefix
        .iter()
        .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
    ensure!(len != 0, "empty H.264 NAL");
    let unit = data
        .get(len_size..len_size + len)
        .context("truncated H.264 NAL")?;
    *data = &data[len_size + len..];
    Ok(unit)
}
fn write_nal(out: &mut Vec<u8>, nal: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
}
fn length_units(mut input: &[u8], size: usize) -> Result<Vec<&[u8]>> {
    let mut nals = Vec::new();
    while !input.is_empty() {
        nals.push(length_unit(&mut input, size)?);
    }
    Ok(nals)
}
impl ParameterSets {
    fn cache(&mut self, nal: &[u8]) {
        match nal[0] & 31 {
            7 => self.sps = nal.to_owned(),
            8 => self.pps = nal.to_owned(),
            _ => {}
        }
    }
    pub fn config(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes[0] != 1 {
            for unit in annex_units(bytes)? {
                self.cache(unit);
            }
            return Ok(());
        }
        ensure!(bytes.len() >= 7, "truncated AVC decoder configuration");
        self.length_bytes = Some(usize::from(bytes[4] & 3) + 1);
        let mut input = &bytes[6..];
        for _ in 0..(bytes[5] & 31) {
            self.cache(length_unit(&mut input, 2)?);
        }
        let (&count, mut input) = input.split_first().context("missing AVC PPS count")?;
        for _ in 0..count {
            self.cache(length_unit(&mut input, 2)?);
        }
        Ok(())
    }
    pub fn packet(&mut self, bytes: &[u8]) -> Result<(bool, Vec<u8>)> {
        ensure!(!bytes.is_empty(), "empty H.264 output");
        let prefix = start_code(bytes);
        let declared = self
            .length_bytes
            .and_then(|len| length_units(bytes, len).ok());
        let units = if let Some(units) = declared {
            units
        } else if prefix.is_some_and(|(i, _)| bytes[..i].iter().all(|b| *b == 0)) {
            annex_units(bytes)?
        } else {
            length_units(bytes, self.length_bytes.unwrap_or(4))?
        };
        let mut idr = false;
        for unit in &units {
            self.cache(unit);
            idr |= unit[0] & 31 == 5;
        }
        let mut result = Vec::with_capacity(bytes.len() + self.sps.len() + self.pps.len() + 8);
        if idr {
            ensure!(
                !self.sps.is_empty() && !self.pps.is_empty(),
                "IDR without SPS/PPS"
            );
            write_nal(&mut result, &self.sps);
            write_nal(&mut result, &self.pps);
        }
        for unit in units {
            // For IDRs the canonical parameter sets already open the access unit.
            if !idr || !matches!(unit[0] & 31, 7..=9) {
                write_nal(&mut result, unit);
            }
        }
        Ok((idr, result))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn converts_avcc_and_repeats_parameters_for_every_idr() {
        let mut sets = ParameterSets::default();
        sets.config(&[1, 66, 0, 30, 0xff, 0xe1, 0, 2, 0x67, 10, 1, 0, 2, 0x68, 20])
            .unwrap();
        let expected = vec![
            0, 0, 0, 1, 0x67, 10, 0, 0, 0, 1, 0x68, 20, 0, 0, 0, 1, 0x65, 1,
        ];
        assert_eq!(
            sets.packet(&[0, 0, 0, 2, 0x65, 1]).unwrap(),
            (true, expected.clone())
        );
        assert_eq!(sets.packet(&[0, 0, 1, 0x65, 1]).unwrap(), (true, expected));
    }
    #[test]
    fn refreshes_in_band_parameters_and_validates_boundaries() {
        let mut sets = ParameterSets::default();
        assert!(sets.packet(&[0, 0, 1, 0x65]).is_err());
        let (key, bytes) = sets
            .packet(&[0, 0, 1, 0x67, 9, 0, 0, 1, 0x68, 3, 0, 0, 1, 0x65, 4])
            .unwrap();
        assert!(key);
        assert_eq!(
            annex_units(&bytes).unwrap(),
            [&[0x67, 9], &[0x68, 3], &[0x65, 4]]
        );
        assert!(sets.packet(&[0, 0, 0, 9, 0x41]).is_err());
    }
}
