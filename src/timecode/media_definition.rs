use super::lookup_table::Lut;

pub const SWITCH_PHASE: u32 = 0x1; // tone phase difference of 270 (not 90) degrees
pub const SWITCH_PRIMARY: u32 = 0x2; // use left channel (not right) as primary
pub const SWITCH_POLARITY: u32 = 0x4; // read bit values in negative (not positive)

#[derive(Debug, Clone)]
pub struct TimecodeDef {
    pub name: String,
    pub desc: String,
    pub resolution: u32,
    pub bits: u32,
    pub flags: u32,
    pub seed: u32,
    pub taps: u32,
    pub length: u32,
    pub safe: u32,
    pub lookup: bool,
    pub lut: Lut,
}

impl TimecodeDef {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: &str,
        desc: &str,
        bits: u32,
        resolution: u32,
        flags: u32,
        seed: u32,
        taps: u32,
        length: u32,
        safe: u32,
    ) -> Self {
        let mut def = Self {
            name: name.to_string(),
            desc: desc.to_string(),
            resolution,
            bits,
            flags,
            seed,
            taps,
            length,
            safe,
            lookup: false,
            lut: Lut::new(length as usize),
        };
        def.build_lut();
        def
    }

    pub fn serato_cd() -> Self {
        Self::new(
            "serato_cd",
            "Serato CD",
            20,
            1000,
            0,
            0xd8b40,
            0x34d54,
            950000,
            940000,
        )
    }

    pub fn serato_2a() -> Self {
        Self::new(
            "serato_2a",
            "Serato 2nd Ed., side A",
            20,
            1000,
            0,
            0x59017,
            0x361e4,
            712000,
            707000,
        )
    }

    pub fn serato_2b() -> Self {
        Self::new(
            "serato_2b",
            "Serato 2nd Ed., side B",
            20,
            1000,
            0,
            0x8f3c6,
            0x4f0d8,
            922000,
            917000,
        )
    }

    pub fn traktor_a() -> Self {
        Self::new(
            "traktor_a",
            "Traktor Scratch, side A",
            23,
            2000,
            SWITCH_PRIMARY | SWITCH_POLARITY | SWITCH_PHASE,
            0x134503,
            0x041040,
            1500000,
            1480000,
        )
    }

    pub fn traktor_b() -> Self {
        Self::new(
            "traktor_b",
            "Traktor Scratch, side B",
            23,
            2000,
            SWITCH_PRIMARY | SWITCH_POLARITY | SWITCH_PHASE,
            0x32066c,
            0x041040,
            2110000,
            2090000,
        )
    }

    pub fn mixvibes_v2() -> Self {
        Self::new(
            "mixvibes_v2",
            "MixVibes V2",
            20,
            1300,
            SWITCH_PHASE,
            0x22c90,
            0x00008,
            950000,
            923000,
        )
    }

    pub fn mixvibes_7inch() -> Self {
        Self::new(
            "mixvibes_7inch",
            "MixVibes 7\"",
            20,
            1300,
            SWITCH_PHASE,
            0x22c90,
            0x00008,
            312000,
            310000,
        )
    }

    pub fn pioneer_a() -> Self {
        Self::new(
            "pioneer_a",
            "Pioneer RekordBox DVS Control Vinyl, side A",
            20,
            1000,
            SWITCH_POLARITY,
            0x78370,
            0x7933a,
            635000,
            614000,
        )
    }

    pub fn pioneer_b() -> Self {
        Self::new(
            "pioneer_b",
            "Pioneer RekordBox DVS Control Vinyl, side B",
            20,
            1000,
            SWITCH_POLARITY,
            0xf7012,
            0x2ef1c,
            918500,
            913000,
        )
    }

    fn lfsr(&self, code: u32, taps: u32) -> u32 {
        let mut taken = code & taps;
        let mut xrs = 0;

        while taken != 0 {
            xrs += taken & 0x1;
            taken >>= 1;
        }

        xrs & 0x1
    }

    pub fn fwd(&self, current: u32) -> u32 {
        let l = self.lfsr(current, self.taps | 0x1);
        (current >> 1) | (l << (self.bits - 1))
    }

    pub fn rev(&self, current: u32) -> u32 {
        let mask = (1 << self.bits) - 1;
        let l = self.lfsr(current, (self.taps >> 1) | (0x1 << (self.bits - 1)));
        ((current << 1) & mask) | l
    }

    pub fn lut_lookup(&self, bitstream: u32) -> Option<i32> {
        self.lut.get(&bitstream)
    }

    pub fn find_definition(name: &str) -> Option<Self> {
        match name {
            "serato_cd" => Some(Self::serato_cd()),
            "serato_2a" => Some(Self::serato_2a()),
            "serato_2b" => Some(Self::serato_2b()),
            "traktor_a" => Some(Self::traktor_a()),
            "traktor_b" => Some(Self::traktor_b()),
            "mixvibes_v2" => Some(Self::mixvibes_v2()),
            "mixvibes_7inch" => Some(Self::mixvibes_7inch()),
            "pioneer_a" => Some(Self::pioneer_a()),
            "pioneer_b" => Some(Self::pioneer_b()),
            _ => None,
        }
    }

    fn build_lut(&mut self) {
        println!("Building lookup table for {} timecode...", self.name);
        println!("Seed: {:08x}, Taps: {:08x}", self.seed, self.taps);

        let mut timecode = self.seed;
        let mut n = 0;

        // Build forwards
        while n < self.length {
            if self.lut.get(&timecode).is_none() {
                self.lut.insert(timecode, n as i32);
                if n % 100000 == 0 {
                    println!(
                        "Forward progress: {} positions, current timecode: {:08x}",
                        n, timecode
                    );
                }
                n += 1;
            }
            timecode = self.fwd(timecode);
            if timecode == self.seed {
                break;
            }
        }

        println!("Forward scan complete: {} positions", n);

        // Build backwards
        timecode = self.rev(self.seed);
        while timecode != self.seed && n < self.length {
            if self.lut.get(&timecode).is_none() {
                self.lut.insert(timecode, -(n as i32));
                if n % 100000 == 0 {
                    println!(
                        "Backward progress: {} positions, current timecode: {:08x}",
                        n, timecode
                    );
                }
                n += 1;
            }
            timecode = self.rev(timecode);
        }

        println!("Lookup table complete: {} total positions", n);
        self.lookup = true;
    }
}

lazy_static::lazy_static! {
    static ref TIMECODES: Vec<TimecodeDef> = vec![
        TimecodeDef::new(
            "serato_2a",
            "Serato 2nd Ed., side A",
            20,
            1000,
            0,
            0x59017,
            0x361e4,
            712000,
            707000,
        ),
        TimecodeDef::new(
            "serato_2b",
            "Serato 2nd Ed., side B",
            20,
            1000,
            0,
            0x8f3c6,
            0x4f0d8,
            922000,
            917000,
        ),
        TimecodeDef::new(
            "serato_cd",
            "Serato CD",
            20,
            1000,
            0,
            0xd8b40,
            0x34d54,
            950000,
            940000,
        ),
        TimecodeDef::new(
            "traktor_a",
            "Traktor Scratch, side A",
            23,
            2000,
            SWITCH_PRIMARY | SWITCH_POLARITY | SWITCH_PHASE,
            0x134503,
            0x041040,
            1500000,
            1480000,
        ),
        TimecodeDef::new(
            "traktor_b",
            "Traktor Scratch, side B",
            23,
            2000,
            SWITCH_PRIMARY | SWITCH_POLARITY | SWITCH_PHASE,
            0x32066c,
            0x041040,
            2110000,
            2090000,
        ),
        TimecodeDef::new(
            "mixvibes_v2",
            "MixVibes V2",
            20,
            1300,
            SWITCH_PHASE,
            0x22c90,
            0x00008,
            950000,
            923000,
        ),
        TimecodeDef::new(
            "mixvibes_7inch",
            "MixVibes 7\"",
            20,
            1300,
            SWITCH_PHASE,
            0x22c90,
            0x00008,
            312000,
            310000,
        ),
        TimecodeDef::new(
            "pioneer_a",
            "Pioneer RekordBox DVS Control Vinyl, side A",
            20,
            1000,
            SWITCH_POLARITY,
            0x78370,
            0x7933a,
            635000,
            614000,
        ),
        TimecodeDef::new(
            "pioneer_b",
            "Pioneer RekordBox DVS Control Vinyl, side B",
            20,
            1000,
            SWITCH_POLARITY,
            0xf7012,
            0x2ef1c,
            918500,
            913000,
        ),
    ];
}

#[allow(unused)]
pub fn find_definition(name: &str) -> Option<TimecodeDef> {
    TIMECODES
        .iter()
        .find(|def| def.name == name)
        .cloned()
        .map(|mut def| {
            def.lookup = true;
            def
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lfsr_fwd_rev_round_trip() {
        // For each timecode definition, fwd then rev should return to the original
        let defs = [
            TimecodeDef::serato_cd(),
            TimecodeDef::serato_2a(),
            TimecodeDef::traktor_a(),
            TimecodeDef::pioneer_a(),
        ];

        for def in &defs {
            let seed = def.seed;
            let fwd = def.fwd(seed);
            let back = def.rev(fwd);
            assert_eq!(
                back, seed,
                "{}: rev(fwd(seed)) != seed (got {:08x}, expected {:08x})",
                def.name, back, seed
            );
        }
    }

    #[test]
    fn test_lfsr_sequence_does_not_degenerate() {
        // The LFSR should not produce zero or get stuck in a short cycle
        let def = TimecodeDef::serato_cd();
        let mut state = def.seed;
        for i in 0..1000 {
            state = def.fwd(state);
            assert_ne!(
                state, 0,
                "{}: LFSR degenerated to zero at step {}",
                def.name, i
            );
        }
    }

    #[test]
    fn test_lut_lookup_finds_seed_at_zero() {
        let def = TimecodeDef::serato_cd();
        let pos = def.lut_lookup(def.seed);
        assert_eq!(pos, Some(0), "seed should map to position 0");
    }
}
