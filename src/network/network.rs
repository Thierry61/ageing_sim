use std::collections::BTreeMap;
use std::fmt;
use std::mem;
use std::iter::{Iterator, Sum};
use std::f64;
use random::{random, shuffle};
use network::prefix::Prefix;
use network::node::Node;
use network::section::Section;
use network::churn::{NetworkEvent, SectionEvent};
use params::Params;
use stats::Stats;

/// A wrapper struct that handles merges in progress
/// When two sections merge, they need to handle a bunch
/// of churn events before they actually become a single
/// section. This remembers which sections are in the
/// process of merging and reports whether all of them are
/// ready to be combined.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PendingMerge {
    complete: BTreeMap<Prefix, bool>,
}

impl PendingMerge {
    /// Creates a new "pending merge" from a set of prefixes - the prefixes passed
    /// are the ones that are supposed to merge
    fn from_prefixes<I: IntoIterator<Item = Prefix>>(pfxs: I) -> Self {
        PendingMerge {
            complete: pfxs.into_iter().map(|pfx| (pfx, false)).collect(),
        }
    }

    /// Mark a prefix as having completed the merge
    fn completed(&mut self, pfx: Prefix) {
        if let Some(entry) = self.complete.get_mut(&pfx) {
            *entry = true;
        }
    }

    /// Returns whether the sections are ready to be combined into one
    fn is_done(&self) -> bool {
        self.complete.iter().all(|(_, &complete)| complete)
    }

    /// Throws out the wrapper layer and returns the pure map
    fn into_map(self) -> BTreeMap<Prefix, bool> {
        self.complete
    }
}

#[derive(Clone, Default)]
pub struct NetworkStructure {
    pub size: usize,
    pub sections: usize,
    pub complete: usize,
}

#[derive(Clone, Default)]
pub struct Output {
    /// the number of "add" random events
    pub adds: u64,
    /// the number of "drop" random events
    pub drops: u64,
    /// the distribution of drops by age
    pub drops_dist: BTreeMap<u8, usize>,
    /// the number of "rejoin" random events
    pub rejoins: u64,
    /// the number of relocations
    pub relocations: u64,
    /// the number of rejected nodes
    pub rejections: u64,
    /// the total number of churn events
    pub churn: u64,
    /// the structure of the network
    pub network_structure: Vec<NetworkStructure>,
}

/// The structure representing the whole network
/// It's a container for sections that simulates all the
/// churn and communication between them.
#[derive(Clone)]
pub struct Network {
    /// all the sections in the network indexed by prefixes
    nodes: BTreeMap<Prefix, Section>,
    /// the nodes that left the network and could rejoin in the future
    left_nodes: Vec<Node>,
    /// queues of events to be processed by each section
    event_queue: BTreeMap<Prefix, Vec<NetworkEvent>>,
    /// prefixes that are in the process of merging
    pending_merges: BTreeMap<Prefix, PendingMerge>,
    /// Simulation parameters
    params: Params,
    /// Simulation outputs
    output: Output,
}

impl Network {
    /// Starts a new network
    pub fn new(params: Params) -> Network {
        let mut nodes = BTreeMap::new();
        nodes.insert(Prefix::empty(), Section::new(Prefix::empty()));
        Network {
            nodes,
            left_nodes: Vec::new(),
            event_queue: BTreeMap::new(),
            pending_merges: BTreeMap::new(),
            params,
            output: Default::default(),
        }
    }

    /// Checks whether there are any events in the queues
    fn has_events(&self) -> bool {
        self.event_queue.values().any(|x| !x.is_empty())
    }

    pub fn capture_network_structure(&mut self) {
        let structure = NetworkStructure {
            size: self.nodes.values().map(|x| x.len()).sum(),
            sections: self.nodes.len(),
            complete: self.nodes.values().filter(|x| x.is_complete()).count(),
        };
        self.output.network_structure.push(structure);
    }

    /// Sends all events to the corresponding sections and processes the events passed
    /// back. The responses generate new events and the cycle continues until the queues are empty.
    /// Then. if any pending merges are ready, they are processed, too.
    pub fn process_events(&mut self) {
        while self.has_events() {
            let queue = mem::replace(&mut self.event_queue, BTreeMap::new());
            for (prefix, events) in queue {
                let mut section_events = vec![];
                for event in events {
                    let params = &self.params;
                    let result = self.nodes
                        .get_mut(&prefix)
                        .map(|section| section.handle_event(event, params))
                        .unwrap_or_else(Vec::new);
                    section_events.extend(result);
                    if let NetworkEvent::PrefixChange(pfx) = event {
                        if let Some(pending_merge) = self.pending_merges.get_mut(&pfx) {
                            pending_merge.completed(prefix);
                        }
                    }
                }
                for section_event in section_events {
                    self.process_single_event(prefix, section_event);
                }
            }
        }
        let merges_to_finalise: Vec<_> = self.pending_merges
            .iter()
            .filter(|&(_, pm)| pm.is_done())
            .map(|(pfx, _)| *pfx)
            .collect();
        for pfx in merges_to_finalise {
            info!("Finalising a merge into {:?}", pfx);
            self.output.churn += 1; // counting merge as a single churn event
            let pending_merge = self.pending_merges.remove(&pfx).unwrap().into_map();
            let mut merged_section = self.merged_section(pending_merge.keys(), true);
            merged_section.recompute_drop_weight(&self.params);
            self.nodes.insert(merged_section.prefix(), merged_section);
        }
        // self.capture_network_structure();
    }

    /// Processes a single response from a section and potentially inserts some events into its
    /// queue
    fn process_single_event(&mut self, prefix: Prefix, event: SectionEvent) {
        match event {
            SectionEvent::NodeDropped(node) => {
                self.left_nodes.push(node);
            }
            SectionEvent::NeedRelocate(node) => {
                self.relocate(node);
            }
            SectionEvent::NodeRejected(_) => {
                self.output.rejections += 1;
            }
            SectionEvent::RequestMerge => {
                self.merge(prefix);
            }
            SectionEvent::RequestSplit => {
                if let Some(section) = self.nodes.remove(&prefix) {
                    let ((mut sec0, ev0), (mut sec1, ev1)) = section.split();
                    let _ = self.event_queue.remove(&prefix);
                    self.event_queue
                        .entry(sec0.prefix())
                        .or_insert_with(Vec::new)
                        .extend(ev0);
                    self.event_queue
                        .entry(sec1.prefix())
                        .or_insert_with(Vec::new)
                        .extend(ev1);
                    sec0.recompute_drop_weight(&self.params);
                    self.nodes.insert(sec0.prefix(), sec0);
                    sec1.recompute_drop_weight(&self.params);
                    self.nodes.insert(sec1.prefix(), sec1);
                    self.output.churn += 1; // counting the split as one churn event
                }
            }
        }
    }

    /// Returns the section that would be the result of merging sections with the given prefixes.
    /// If `destructive` is true, the sections are actually removed from `self.nodes` to be
    /// combined.
    fn merged_section<'a, I: IntoIterator<Item = &'a Prefix> + Clone>(
        &mut self,
        prefixes: I,
        destructive: bool,
    ) -> Section {
        let mut sections: Vec<_> = prefixes
            .clone()
            .into_iter()
            .filter_map(|pfx| {
                if destructive {
                    let _ = self.event_queue.remove(pfx);
                    self.nodes.remove(pfx)
                } else {
                    self.nodes.get(pfx).cloned()
                }
            })
            .collect();

        while sections.len() > 1 {
            sections.sort_by_key(|s| s.prefix().len());
            let section1 = sections.pop().unwrap();
            let section2 = sections.pop().unwrap();
            let section = section1.merge(section2, &self.params);
            sections.push(section);
        }

        sections.pop().unwrap()
    }

    /// Calculates which sections will merge into a given prefix, creates a pending merge for them
    /// and prepares queues for churn events to be processed before the merge itself.
    fn merge(&mut self, prefix: Prefix) {
        let merged_pfx = prefix.shorten();
        if let Some(&compatible_merge) = self.pending_merges
            .keys()
            .find(|pfx| pfx.is_compatible_with(&merged_pfx))
        {
            if compatible_merge.is_ancestor(&merged_pfx) {
                return;
            }
            let _ = self.pending_merges.remove(&compatible_merge);
        }
        info!("Initiating a merge into {:?}", merged_pfx);
        let prefixes: Vec<_> = self.nodes
            .keys()
            .filter(|&pfx| merged_pfx.is_ancestor(pfx))
            .cloned()
            .collect();

        let pending_merge = PendingMerge::from_prefixes(prefixes.iter().cloned());
        self.pending_merges.insert(merged_pfx, pending_merge);

        let merged_section = self.merged_section(prefixes.iter(), false);
        for pfx in prefixes {
            let events = self.calculate_merge_events(&merged_section, pfx);
            let _ = self.event_queue.insert(pfx, events);
        }
    }

    /// Creates the queue of events to be processed by a section `pfx` when it merges into
    /// `merged`.
    fn calculate_merge_events(&self, merged: &Section, pfx: Prefix) -> Vec<NetworkEvent> {
        let old_elders = self.nodes.get(&pfx).unwrap().elders();
        let new_elders = merged.elders();
        let mut events = vec![NetworkEvent::StartMerge(merged.prefix())];
        for lost_elder in &old_elders - &new_elders {
            events.push(NetworkEvent::Gone(lost_elder));
        }
        for gained_elder in &new_elders - &old_elders {
            events.push(NetworkEvent::Live(gained_elder, false));
        }
        events.push(NetworkEvent::PrefixChange(merged.prefix()));
        events
    }

    /// Adds a random node to the network by pushing an appropriate event to the queue
    pub fn add_random_node(&mut self) {
        self.output.adds += 1;
        self.output.churn += 1;
        let node = Node::new(random(), self.params.init_age);
        info!("Adding node {:?}", node);
        let prefix = self.prefix_for_node(node);
        self.event_queue
            .entry(prefix)
            .or_insert_with(Vec::new)
            .push(NetworkEvent::Live(node, true));
    }

    /// Calculates the sum of weights for the dropping probability.
    /// When choosing the node to be dropped, every node is assigned a weight, so that older nodes
    /// have less chance of dropping. This helps in calculating which node should be dropped.
    fn total_drop_weight(&self) -> f64 {
        self.nodes
            .iter()
            .map(|(_, s)| s.drop_weight())
            .sum()
    }

    /// Returns the prefix a node should belong to.
    fn prefix_for_node(&self, node: Node) -> Prefix {
        // Use reverse iterator from node name to get section prefix
        let max = Prefix::from_name(&node.name());
        let pfx = self.nodes.range(..max).next_back().map(|(pfx, _)| pfx.clone()).unwrap();
        // Check that the algorithm is correct
        assert!(
            pfx.matches(node.name()),
            "Section {:?} does not match {:?}!",
            pfx,
            node.name()
        );
        pfx
    }

    /// Chooses a new section for the given node, generates a new name for it,
    /// increases its age,  and sends a `Live` event to the section.
    fn relocate(&mut self, node: Node) {
        self.output.relocations += 1;
        self.output.churn += 2; // leaving one section and joining another one
        let (node, neighbour) = {
            // Choose a complete random name, then get its section and lastly select its weakest neighbour.
            let mut new_node = if random::<f64>() < self.params.distant_relocation_probability {
                Node::new(random(), node.age())
            } else {
                node.clone()
            };
            let src_section = self.prefix_for_node(new_node);
            // Neighbours are sections having one bit difference. They can be shorter or longer
            // but we exclude longer ones because they are in better shape.
            let mut neighbours: Vec<Prefix> = Vec::new();
            let len = src_section.len();
            for pos in 0..len {
                let mut pfx = src_section.with_flipped_bit(pos);
                for _ in 0..len-pos {
                    if self.nodes.contains_key(&pfx) {
                        // Check that the algorithm is correct
                        assert!(
                            pfx.is_neighbour(&src_section),
                            "Section {:?} is not neighbour of {:?}!",
                            pfx,
                            src_section
                        );
                        neighbours.push(pfx.clone());
                        // A shorter prefix cannot exist
                        break;
                    }
                    pfx = pfx.shorten();
                }
            }
            // Add src_section itself
            neighbours.push(src_section.clone());
            // relocate to the neighbour first with the shortest prefix and then the least peers as per the document
            neighbours.sort_by_key(|pfx| pfx.len() as usize * 10000 + self.nodes.get(pfx).unwrap().len());
            let neighbour = if let Some(n) = neighbours.first() {
                n
            } else {
                &src_section
            };
            // Choose in which half of the section we relocate the node (to balance the section)
            let (count0, count1) = self.nodes.get(&neighbour).unwrap().count_halves(&self.params);
            let bit: Option<u8> = if count0 == count1 { None} else if count0 > count1 { Some(1) } else { Some(0) };
            new_node.relocate(neighbour, bit);
            info!(
                "Relocating {:?} from {:?} to {:?} as {:?}",
                node, src_section, neighbour, new_node
            );
            (new_node, neighbour.clone())
        };
        self.event_queue
            .entry(neighbour)
            .or_insert_with(Vec::new)
            .push(NetworkEvent::Live(node, true));
    }

    /// Drops a random node from the network by sending a `Lost` event to the section.
    /// The probability of a given node dropping is weighted based on its age.
    pub fn drop_random_node(&mut self) {
        self.output.drops += 1;
        self.output.churn += 1;
        let total_weight = self.total_drop_weight();
        let mut drop = random::<f64>() * total_weight;
        let prefix_and_section = {
            let mut res = None;
            for (p, s) in &self.nodes {
                if s.drop_weight() > drop {
                    res = Some((p, s));
                    break;
                }
                drop -= s.drop_weight();
            }
            res
        };
        if let Some((prefix, section)) = prefix_and_section {
            let node = {
                let mut res = None;
                for n in section.nodes().into_iter() {
                    if n.drop_probability(self.params.drop_dist) > drop {
                        res = Some(n);
                        break;
                    }
                    drop -= n.drop_probability(self.params.drop_dist);
                }
                res
            };
            if let Some(node) = node {
                *self.output.drops_dist.entry(node.age()).or_insert(0) += 1;
                let name = node.name();
                info!("Dropping node {:?} from section {:?}", name, prefix);
                self.event_queue
                    .entry(*prefix)
                    .or_insert_with(Vec::new)
                    .push(NetworkEvent::Lost(name));
            }
        }
    }

    /// Chooses a random node from among the ones that left the network and gets it to rejoin.
    /// The age of the rejoining node is reduced.
    pub fn rejoin_random_node(&mut self) {
        self.output.rejoins += 1;
        self.output.churn += 1;
        shuffle(&mut self.left_nodes);
        if let Some(mut node) = self.left_nodes.pop() {
            info!("Rejoining node {:?}", node);
            node.rejoined(self.params.init_age);
            let prefix = self.prefix_for_node(node);
            self.event_queue
                .entry(prefix)
                .or_insert_with(Vec::new)
                .push(NetworkEvent::Live(node, true));
        }
    }

    pub fn num_sections(&self) -> usize {
        self.nodes.len()
    }

    pub fn age_distribution(&self) -> BTreeMap<u8, usize> {
        let mut result = BTreeMap::new();
        for (_, section) in &self.nodes {
            for node in section.nodes() {
                *result.entry(node.age()).or_insert(0) += 1;
            }
        }
        result
    }

    pub fn complete_sections(&self) -> usize {
        self.nodes.iter().filter(|&(_, s)| s.is_complete()).count()
    }

    pub fn output(&self) -> &Output {
        &self.output
    }
}

impl fmt::Debug for Network {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(
            fmt,
            "Network {{\n\tadds: {}\n\tdrops: {}\n\trejoins: {}\n\trelocations: {}\n\trejections: {}\n\ttotal churn: {}\n\ttotal nodes: {}\n\n{:?}\nleft_nodes: {:?}\n\n}}",
            self.output.adds,
            self.output.drops,
            self.output.rejoins,
            self.output.relocations,
            self.output.rejections,
            self.output.churn,
            usize::sum(self.nodes.values().map(|s| s.len())),
            self.nodes.values(),
            self.left_nodes
        )
    }
}

// Display network summary as a markdown table
impl fmt::Display for Network {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let sections = self.num_sections();
        let rejecting = self.nodes.iter().filter(|&(_, s)| s.reject_young_node(&self.params)).count() as f64;
        // Network summary
        try!(writeln!(fmt, "|    Metrics     |  Values  |"));
        try!(writeln!(fmt, "|:---------------|---------:|"));
        try!(writeln!(fmt, "| Adds           | {:>8} |", self.output.adds));
        try!(writeln!(fmt, "| Drops          | {:>8} |", self.output.drops));
        try!(writeln!(fmt, "| Rejoins        | {:>8} |", self.output.rejoins));
        try!(writeln!(fmt, "| Relocations    | {:>8} |", self.output.relocations));
        try!(writeln!(fmt, "| Rejections     | {:>8} |", self.output.rejections));
        try!(writeln!(fmt, "| Churns         | {:>8} |", self.output.churn));
        try!(writeln!(fmt, "| Sections       | {:>8} |", sections));
        let complete = self.complete_sections();
        if complete != sections {
            try!(writeln!(fmt, "| Complete       | {:>8} |", complete));
        }
        try!(writeln!(fmt, "| Section nodes  | {:>8} |", usize::sum(self.nodes.values().map(|s| s.len()))));
        try!(writeln!(fmt, "| Left nodes     | {:>8} |", self.left_nodes.len()));
        try!(writeln!(fmt, "| Rejection rate | {:>7.0}% |", rejecting / sections as f64 * 100.0));

        // Distribution of sections per prefix length
        let mut distribution : BTreeMap<u8, Vec<usize>> = BTreeMap::new();
        for (pfx, section) in &self.nodes {
            let mut entry = distribution.entry(pfx.len()).or_insert(Vec::new());
            entry.push(section.len());
        }
        let mut lengths: Vec<u8> = distribution.keys().cloned().collect();
        lengths.sort();
        try!(writeln!(fmt, "| Prefix lengths | {:>8} |", lengths.len()));
        let max_prefix_length = lengths.last().cloned().unwrap();
        let mut max_density = f64::MIN_POSITIVE;
        let mut min_density = f64::MAX;
        for (pfx, section) in &self.nodes {
            let range = max_prefix_length - pfx.len();
            let width = 1 << range;
            let density = section.len() as f64 / width as f64;
            max_density = max_density.max(density);
            min_density = min_density.min(density);
        }
        try!(writeln!(fmt, "| Density gap    | {:>8.2} |", max_density / min_density));
        try!(writeln!(fmt));

        try!(writeln!(fmt, "| Prefix len {}", Stats::get_header_line()));
        try!(writeln!(fmt, "|-----------:{}", Stats::get_separator_line()));
        for i in lengths {
            try!(writeln!(fmt, "| {:>10} | {}", i, Stats::new(distribution.get(&i).unwrap())))
        }
        writeln!(fmt, "|        All | {}", Stats::new(&self.nodes.values().map(|s| s.len()).collect()))
    }
}
