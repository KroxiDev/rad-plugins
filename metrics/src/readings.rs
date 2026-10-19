//! Parsers accept only bounded snapshots supplied by the reader.
#[derive(Clone, Copy, Debug)]
pub struct Cpu([u64; 8]);
impl Cpu {
    pub fn parse(text: &str) -> Option<Self> {
        let mut fields = text.lines().next()?.split_whitespace();
        if fields.next()? != "cpu" {
            return None;
        }
        let mut values = [0; 8];
        for value in &mut values {
            *value = fields.next()?.parse().ok()?;
        }
        Some(Self(values))
    }
    pub fn percent_since(self, previous: Self) -> Option<u32> {
        let mut total = 0u64;
        let mut idle = 0u64;
        for (i, (now, old)) in self.0.into_iter().zip(previous.0).enumerate() {
            let delta = now.checked_sub(old)?;
            total = total.checked_add(delta)?;
            if i == 3 || i == 4 {
                idle = idle.checked_add(delta)?;
            }
        }
        if total == 0 {
            return None;
        }
        Some(((u128::from(total - idle) * 100) / u128::from(total)) as u32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    pub used: u64,
    pub available: u64,
    pub total: u64,
}
impl Memory {
    pub fn parse(text: &str) -> Option<Self> {
        let (mut total, mut available) = (None, None);
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let slot = match fields.next() {
                Some("MemTotal:") => &mut total,
                Some("MemAvailable:") => &mut available,
                _ => continue,
            };
            if slot.is_some() {
                return None;
            }
            let kib: u64 = fields.next()?.parse().ok()?;
            if fields.next()? != "kB" || fields.next().is_some() {
                return None;
            }
            *slot = Some(kib.checked_mul(1024)?);
        }
        let (total, available) = (total?, available?);
        if total == 0 {
            return None;
        }
        Some(Self {
            used: total.checked_sub(available)?,
            available,
            total,
        })
    }
}
pub const MAX_PROC_BYTES: u64 = 65536;
pub fn read_bounded(reader: impl std::io::Read) -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    reader.take(MAX_PROC_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PROC_BYTES as usize {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    String::from_utf8(bytes).map_err(|_| std::io::ErrorKind::InvalidData.into())
}
