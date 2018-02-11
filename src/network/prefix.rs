use std::fmt;

/// A helper struct that only has the purpose of pretty-printing debug information
#[derive(Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Serialize, Deserialize)]
pub struct Name(pub u64);

impl fmt::Debug for Name {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let (b0, b1, b2) = (
            (self.0 >> 56) as u8,
            (self.0 >> 48) as u8,
            (self.0 >> 40) as u8,
        );
        write!(fmt, "{:02x}{:02x}{:02x}...", b0, b1, b2)
    }
}

/// A structure representing a network prefix - a simplified version of the Prefix struct from
/// bits field put before len field so that prefixes are ordered correctly in btree maps
/// `routing`
#[derive(Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefix {
    bits: u64,
    len: u8,
}

impl Prefix {
    /// Build a prefix from name, with len set to 255
    pub fn from_name(name: &Name) -> Prefix {
        Prefix { bits: name.0, len: <u8>::max_value() }
    }

    pub fn empty() -> Prefix {
        Prefix { bits: 0, len: 0 }
    }

    pub fn extend(self, bit: u8) -> Prefix {
        if self.len > 63 {
            return self;
        }
        let bit = (bit as u64 & 1) << (63 - self.len);
        Prefix {
            bits: self.bits | bit,
            len: self.len + 1,
        }
    }

    pub fn len(&self) -> u8 {
        self.len
    }

    // Generate a mask with len highest bits set to 1, for example 11110000 ... 00000000 if len == 4
    fn len_mask(&self) -> u64 {
        if self.len == 0 {
            0
        } else {
            (-1i64 as u64) << (64 - self.len)
        }
    }

    pub fn shorten(self) -> Prefix {
        if self.len < 1 {
            return self;
        }
        let mask = self.len_mask() << 1;
        Prefix {
            bits: self.bits & mask,
            len: self.len - 1,
        }
    }

    pub fn with_flipped_bit(self, bit: u8) -> Prefix {
        let mask = 1 << (63 - bit);
        Prefix {
            bits: self.bits ^ mask,
            len: self.len,
        }
    }

    pub fn matches(&self, name: Name) -> bool {
        (name.0 & self.len_mask()) ^ self.bits == 0
    }

    pub fn is_ancestor(&self, other: &Prefix) -> bool {
        self.len <= other.len && self.matches(Name(other.bits))
    }

    #[allow(unused)]
    pub fn is_child(&self, other: &Prefix) -> bool {
        other.is_ancestor(self)
    }

    pub fn is_compatible_with(&self, other: &Prefix) -> bool {
        self.is_ancestor(other) || self.is_child(other)
    }

    pub fn is_sibling(&self, other: &Prefix) -> bool {
        if self.len > 0 {
            (*self).with_flipped_bit(self.len - 1) == *other
        } else {
            false
        }
    }

    pub fn is_neighbour(&self, other: &Prefix) -> bool {
        let diff = self.bits ^ other.bits;
        let bit = diff.leading_zeros() as u8;
        if bit < self.len && bit < other.len {
            let diff = self.with_flipped_bit(bit).bits ^ other.bits;
            let bit = diff.leading_zeros() as u8;
            bit >= self.len || bit >= other.len
        } else {
            false
        }
    }

    pub fn substituted_in(&self, mut name: Name) -> Name {
        let mask = self.len_mask();
        name.0 &= !mask;
        name.0 |= self.bits;
        name
    }

    #[allow(unused)]
    pub fn from_str(s: &str) -> Option<Prefix> {
        let mut prefix = Self::empty();
        for c in s.chars() {
            match c {
                '0' => {
                    prefix = prefix.extend(0);
                }
                '1' => {
                    prefix = prefix.extend(1);
                }
                _ => {
                    return None;
                }
            }
        }
        Some(prefix)
    }

    pub fn to_string(&self) -> String {
        let mut result = String::new();
        for i in 0..self.len {
            let mask = 1 << (63 - i);
            if self.bits & mask == 0 {
                result.push('0');
            } else {
                result.push('1');
            }
        }
        result
    }
}

impl fmt::Debug for Prefix {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "Prefix({})", self.to_string())
    }
}
