use std::fmt;
use random::random;
use network::prefix::{Name, Prefix};
use params::DropDist;

pub type Digest = [u8; 32];

/// A node has a name and an age
#[derive(Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    name: Name,
    age: u8,
}

impl fmt::Debug for Node {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "Node({:?}; age={})", self.name, self.age)
    }
}

impl Node {
    /// Creates a new node
    pub fn new(name: u64, age: u8) -> Node {
        Node {
            name: Name(name),
            age,
        }
    }

    /// Generates a relocated name and increases the age by 1
    /// bit parameter indicates in which half of the section the node is relocated
    pub fn relocate(&mut self, prefix: &Prefix, bit: Option<u8>) {
        let prefix : Prefix = match bit {
            None => *prefix,
            Some(bit) => prefix.extend(bit),
        };
        self.name = prefix.substituted_in(Name(random()));
        self.age += 1;
    }

    /// Decrement the age, because the node is rejoining
    pub fn rejoined(&mut self, min_age: u8) {
        if self.age > min_age {
            self.age -= 1;
        }
    }

    /// Returns the name
    pub fn name(&self) -> Name {
        self.name
    }

    /// Returns the age
    pub fn age(&self) -> u8 {
        self.age
    }

    /// Returns whether the node is an Adult
    pub fn is_adult(&self) -> bool {
        self.age > 4
    }

    /// Returns the weight used in randomly choosing a node to be dropped
    pub fn drop_probability(&self, dist: DropDist) -> f64 {
        match dist {
            DropDist::RevProp => 10.0 / self.age as f64,
            DropDist::Exponential => 2.0f64.powf(-(self.age as f64)),
        }
    }
}
