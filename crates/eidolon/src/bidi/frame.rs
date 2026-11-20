use std::fmt;

/// The largest payload `parse` accepts by default: 64 MiB, since a
/// screenshot is a large message.
pub const MAX_PAYLOAD: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl Opcode {
    fn from_bits(bits: u8) -> Option<Self> {
        Some(match bits {
            0x0 => Self::Continuation,
            0x1 => Self::Text,
            0x2 => Self::Binary,
            0x8 => Self::Close,
            0x9 => Self::Ping,
            0xA => Self::Pong,
            _ => return None,
        })
    }

    fn bits(self) -> u8 {
        match self {
            Self::Continuation => 0x0,
            Self::Text => 0x1,
            Self::Binary => 0x2,
            Self::Close => 0x8,
            Self::Ping => 0x9,
            Self::Pong => 0xA,
        }
    }

    pub fn is_control(self) -> bool {
        self.bits() >= 0x8
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: Opcode,
    pub masked: bool,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    ReservedBits,
    UnknownOpcode(u8),
    ControlTooLong,
    ControlFragmented,
    TooLarge { length: u64, limit: usize },
    BadLength,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedBits => write!(f, "a frame sets reserved bits, which were not negotiated"),
            Self::UnknownOpcode(op) => write!(f, "a frame has the unknown opcode {op:#x}"),
            Self::ControlTooLong => write!(f, "a control frame is longer than 125 bytes"),
            Self::ControlFragmented => write!(f, "a control frame is split over several frames"),
            Self::TooLarge { length, limit } => {
                write!(f, "a frame of {length} bytes is over the limit of {limit} bytes")
            }
            Self::BadLength => write!(f, "a frame has an invalid 64-bit length"),
        }
    }
}

impl std::error::Error for FrameError {}

pub fn parse(buf: &[u8]) -> Result<Option<(Frame, usize)>, FrameError> {
    parse_with_limit(buf, MAX_PAYLOAD)
}

pub fn parse_with_limit(buf: &[u8], limit: usize) -> Result<Option<(Frame, usize)>, FrameError> {
    let [b0, b1, rest @ ..] = buf else {
        return Ok(None);
    };
    if b0 & 0x70 != 0 {
        return Err(FrameError::ReservedBits);
    }
    let fin = b0 & 0x80 != 0;
    let opcode = Opcode::from_bits(b0 & 0x0F).ok_or(FrameError::UnknownOpcode(b0 & 0x0F))?;
    let masked = b1 & 0x80 != 0;
    let (length, mut at) = match b1 & 0x7F {
        126 => match rest {
            [a, b, ..] => (u64::from(u16::from_be_bytes([*a, *b])), 4),
            _ => return Ok(None),
        },
        127 => match rest.first_chunk::<8>() {
            Some(bytes) => {
                let n = u64::from_be_bytes(*bytes);
                if n >> 63 != 0 {
                    return Err(FrameError::BadLength);
                }
                (n, 10)
            }
            None => return Ok(None),
        },
        n => (u64::from(n), 2),
    };
    if opcode.is_control() {
        if !fin {
            return Err(FrameError::ControlFragmented);
        }
        if length > 125 {
            return Err(FrameError::ControlTooLong);
        }
    }
    if length > limit as u64 {
        return Err(FrameError::TooLarge { length, limit });
    }
    // Under the limit, so it fits a usize.
    let length = length as usize;
    let mask = if masked {
        let Some(m) = buf.get(at..at + 4) else {
            return Ok(None);
        };
        at += 4;
        Some([m[0], m[1], m[2], m[3]])
    } else {
        None
    };
    let Some(body) = buf.get(at..at.saturating_add(length)) else {
        return Ok(None);
    };
    let mut payload = body.to_vec();
    if let Some(mask) = mask {
        apply_mask(&mut payload, mask);
    }
    Ok(Some((Frame { fin, opcode, masked, payload }, at + length)))
}

pub fn encode(fin: bool, opcode: Opcode, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(opcode.bits() | if fin { 0x80 } else { 0 });
    let mask_bit = if mask.is_some() { 0x80 } else { 0 };
    let len = payload.len();
    if len < 126 {
        out.push(mask_bit | len as u8);
    } else if let Ok(len) = u16::try_from(len) {
        out.push(mask_bit | 0x7E);
        out.extend_from_slice(&len.to_be_bytes());
    } else {
        out.push(mask_bit | 0x7F);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    match mask {
        Some(mask) => {
            out.extend_from_slice(&mask);
            let start = out.len();
            out.extend_from_slice(payload);
            apply_mask(&mut out[start..], mask);
        }
        None => out.extend_from_slice(payload),
    }
    out
}

pub fn apply_mask(data: &mut [u8], mask: [u8; 4]) {
    for (i, byte) in data.iter_mut().enumerate() {
        *byte ^= mask[i % 4];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASK: [u8; 4] = [0x37, 0xFA, 0x21, 0x3D];

    #[test]
    fn rfc_unmasked_text_hello() {
        // RFC 6455 section 5.7.
        let bytes = [0x81, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f];
        let (frame, used) = parse(&bytes).unwrap().unwrap();
        assert_eq!(used, 7);
        assert!(frame.fin && !frame.masked);
        assert_eq!(frame.opcode, Opcode::Text);
        assert_eq!(frame.payload, b"Hello");
    }

    #[test]
    fn rfc_masked_text_hello() {
        let bytes = [0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58];
        let (frame, used) = parse(&bytes).unwrap().unwrap();
        assert_eq!(used, 11);
        assert!(frame.masked);
        assert_eq!(frame.payload, b"Hello");
    }

    #[test]
    fn encode_masked_matches_rfc() {
        let out = encode(true, Opcode::Text, b"Hello", Some(MASK));
        assert_eq!(out, [0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58]);
    }

    #[test]
    fn truncated_input_asks_for_more() {
        let full = encode(true, Opcode::Text, &[7u8; 300], Some(MASK));
        for cut in 0..full.len() {
            assert_eq!(parse(&full[..cut]), Ok(None), "cut at {cut}");
        }
        assert!(parse(&full).unwrap().is_some());
    }

    #[test]
    fn sixteen_bit_length() {
        let payload = vec![9u8; 126];
        let bytes = encode(true, Opcode::Binary, &payload, None);
        assert_eq!(bytes[1], 126);
        let (frame, used) = parse(&bytes).unwrap().unwrap();
        assert_eq!(used, 4 + 126);
        assert_eq!(frame.payload, payload);
        let big = vec![1u8; 65_535];
        let bytes = encode(true, Opcode::Binary, &big, Some(MASK));
        assert_eq!(bytes[1] & 0x7F, 126);
        assert_eq!(parse(&bytes).unwrap().unwrap().0.payload, big);
    }

    #[test]
    fn sixty_four_bit_length() {
        let payload = vec![3u8; 65_536];
        let bytes = encode(true, Opcode::Binary, &payload, Some(MASK));
        assert_eq!(bytes[1] & 0x7F, 127);
        let (frame, used) = parse(&bytes).unwrap().unwrap();
        assert_eq!(used, 2 + 8 + 4 + 65_536);
        assert_eq!(frame.payload, payload);
    }

    #[test]
    fn rest_of_buffer_is_left_alone() {
        let mut bytes = encode(true, Opcode::Ping, b"a", None);
        bytes.extend_from_slice(&[0x81]);
        let (_, used) = parse(&bytes).unwrap().unwrap();
        assert_eq!(used, 3);
    }

    #[test]
    fn refuses_what_it_cannot_go_on_with() {
        assert_eq!(parse(&[0xC1, 0x00]), Err(FrameError::ReservedBits));
        assert_eq!(parse(&[0x83, 0x00]), Err(FrameError::UnknownOpcode(3)));
        assert_eq!(parse(&[0x09, 0x00]), Err(FrameError::ControlFragmented));
        assert_eq!(parse(&[0x89, 126, 0, 200]), Err(FrameError::ControlTooLong));
        let mut bad = vec![0x82, 127, 0x80, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse(&bad), Err(FrameError::BadLength));
        bad[2] = 0x7F;
        assert!(matches!(parse(&bad), Err(FrameError::TooLarge { .. })));
    }

    #[test]
    fn a_huge_claimed_length_fails_before_its_bytes_arrive() {
        let header = [0x82, 127, 0, 0, 0, 0, 0x10, 0, 0, 0];
        assert_eq!(parse_with_limit(&header, 1024), Err(FrameError::TooLarge { length: 0x1000_0000, limit: 1024 }));
    }

    #[test]
    fn random_bytes_never_panic() {
        // A small xorshift, so the test needs no random crate.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..5_000 {
            let len = (next() % 40) as usize;
            let buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let _ = parse_with_limit(&buf, 1 << 20);
        }
    }
}
