use anyhow::{ensure, Context, Result};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BitstreamCodec {
    #[default]
    H264,
    Hevc,
}

#[derive(Default)]
pub struct ParameterSets {
    codec: BitstreamCodec,
    vps: Vec<u8>,
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
        let (offset, len) = start_code(bytes).context("missing Annex-B start code")?;
        ensure!(
            bytes[..offset].iter().all(|v| *v == 0),
            "invalid Annex-B leading bytes"
        );
        bytes = &bytes[offset + len..];
        let end = start_code(bytes).map_or(bytes.len(), |p| p.0);
        ensure!(end > 0, "empty NAL unit");
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
    ensure!(len != 0, "empty NAL unit");
    let unit = data
        .get(len_size..len_size + len)
        .context("truncated NAL unit")?;
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
    pub fn new(codec: BitstreamCodec) -> Self {
        Self {
            codec,
            ..Default::default()
        }
    }

    fn nal_type(&self, nal: &[u8]) -> u8 {
        match self.codec {
            BitstreamCodec::H264 => nal[0] & 0x1F,
            BitstreamCodec::Hevc => (nal[0] >> 1) & 0x3F,
        }
    }

    fn is_keyframe_nal(&self, nal: &[u8]) -> bool {
        match self.codec {
            BitstreamCodec::H264 => self.nal_type(nal) == 5,
            // HEVC IRAP: IDR_W_RADL (19), IDR_N_LP (20), CRA_NUT (21)
            BitstreamCodec::Hevc => matches!(self.nal_type(nal), 19..=21),
        }
    }

    fn is_header_or_aud(&self, nal: &[u8]) -> bool {
        match self.codec {
            BitstreamCodec::H264 => matches!(self.nal_type(nal), 7..=9),
            // HEVC VPS (32), SPS (33), PPS (34), AUD (35)
            BitstreamCodec::Hevc => matches!(self.nal_type(nal), 32..=35),
        }
    }

    fn cache(&mut self, nal: &[u8]) {
        match self.codec {
            BitstreamCodec::H264 => match nal[0] & 0x1F {
                7 => self.sps = nal.to_owned(),
                8 => self.pps = nal.to_owned(),
                _ => {}
            },
            BitstreamCodec::Hevc => match (nal[0] >> 1) & 0x3F {
                32 => self.vps = nal.to_owned(),
                33 => self.sps = nal.to_owned(),
                34 => self.pps = nal.to_owned(),
                _ => {}
            },
        }
    }

    pub fn config(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if start_code(bytes).is_some_and(|(i, _)| bytes[..i].iter().all(|b| *b == 0)) {
            for unit in annex_units(bytes)? {
                self.cache(unit);
            }
            return Ok(());
        }
        match self.codec {
            BitstreamCodec::H264 => {
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
            BitstreamCodec::Hevc => {
                if bytes[0] != 1 {
                    for unit in annex_units(bytes)? {
                        self.cache(unit);
                    }
                    return Ok(());
                }
                ensure!(bytes.len() >= 23, "truncated HEVC decoder configuration");
                self.length_bytes = Some(usize::from(bytes[21] & 3) + 1);
                let num_arrays = bytes[22];
                let mut input = &bytes[23..];
                for _ in 0..num_arrays {
                    let hdr = input.get(..3).context("truncated hvcC array header")?;
                    let count = u16::from_be_bytes([hdr[1], hdr[2]]);
                    input = &input[3..];
                    for _ in 0..count {
                        self.cache(length_unit(&mut input, 2)?);
                    }
                }
                Ok(())
            }
        }
    }

    pub fn packet(&mut self, bytes: &[u8]) -> Result<(bool, Vec<u8>)> {
        ensure!(!bytes.is_empty(), "empty bitstream output");
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
            idr |= self.is_keyframe_nal(unit);
        }
        let mut result = Vec::with_capacity(
            bytes.len() + self.vps.len() + self.sps.len() + self.pps.len() + 12,
        );
        if idr {
            match self.codec {
                BitstreamCodec::H264 => {
                    ensure!(
                        !self.sps.is_empty() && !self.pps.is_empty(),
                        "H.264 IDR without SPS/PPS"
                    );
                    write_nal(&mut result, &self.sps);
                    write_nal(&mut result, &self.pps);
                }
                BitstreamCodec::Hevc => {
                    ensure!(
                        !self.vps.is_empty() && !self.sps.is_empty() && !self.pps.is_empty(),
                        "HEVC IRAP without VPS/SPS/PPS"
                    );
                    write_nal(&mut result, &self.vps);
                    write_nal(&mut result, &self.sps);
                    write_nal(&mut result, &self.pps);
                }
            }
        }
        for unit in units {
            // For keyframes the canonical parameter sets already open the access unit.
            if !idr || !self.is_header_or_aud(unit) {
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

    #[test]
    fn parses_hevc_hvcc_and_inlines_vps_sps_pps_before_irap() {
        let mut sets = ParameterSets::new(BitstreamCodec::Hevc);
        // Minimal hvcC box header (23 bytes) + 3 arrays: VPS(32), SPS(33), PPS(34)
        // HEVC NAL header bytes: VPS = 32<<1 = 0x40, SPS = 33<<1 = 0x42, PPS = 34<<1 = 0x44, IDR_W_RADL = 19<<1 = 0x26
        let mut hvcc = vec![0u8; 23];
        hvcc[0] = 1; // configurationVersion
        hvcc[21] = 0xFF; // lengthSizeMinusOne = 3 (4 bytes)
        hvcc[22] = 3; // numOfArrays
        hvcc.extend_from_slice(&[32, 0, 1, 0, 3, 0x40, 0x01, 0xAA]);
        hvcc.extend_from_slice(&[33, 0, 1, 0, 3, 0x42, 0x01, 0xBB]);
        hvcc.extend_from_slice(&[34, 0, 1, 0, 3, 0x44, 0x01, 0xCC]);
        sets.config(&hvcc).unwrap();

        let (key, out) = sets
            .packet(&[0, 0, 0, 4, 0x26, 0x01, 0xDE, 0xAD])
            .unwrap();
        assert!(key);
        assert_eq!(
            annex_units(&out).unwrap(),
            [
                &[0x40, 0x01, 0xAA][..],
                &[0x42, 0x01, 0xBB][..],
                &[0x44, 0x01, 0xCC][..],
                &[0x26, 0x01, 0xDE, 0xAD][..],
            ]
        );
        // Non-IDR trailing slice (NAL type 1 = 0x02) must not prepend VPS/SPS/PPS
        let (key2, out2) = sets.packet(&[0, 0, 0, 3, 0x02, 0x01, 0x99]).unwrap();
        assert!(!key2);
        assert_eq!(annex_units(&out2).unwrap(), [&[0x02, 0x01, 0x99][..]]);
    }
}
