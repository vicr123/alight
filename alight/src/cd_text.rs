const CD_TEXT_CRC_ALG: crc::Algorithm<u16> = crc::Algorithm {
    width: 16,
    poly: 0x1021,
    init: 0x0000,
    refin: false,
    refout: false,
    xorout: 0xffff,
    check: 0x29b1,
    residue: 0x0000,
};

#[derive(Debug, Default, Clone)]
pub struct TrackData {
    pub title: Option<String>,
    pub performers: Option<String>,
    pub songwriters: Option<String>,
    pub composers: Option<String>,
    pub arrangers: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct CdText {
    title_data: Vec<Option<String>>,
    performer_data: Vec<Option<String>>,
    songwriter_data: Vec<Option<String>>,
    composer_data: Vec<Option<String>>,
    arranger_data: Vec<Option<String>>,
}

#[derive(Debug, Copy, Clone)]
#[repr(u8)]
enum PackType {
    Title = 0x80,
    Performers = 0x81,
    Songwriters = 0x82,
    Composers = 0x83,
    Arrangers = 0x84,
    MessageArea = 0x85,
    DiscIdentification = 0x86,
    GenreIdentification = 0x87,
    TableOfContents = 0x88,
    TableOfContents2 = 0x89,
    BlockSizeInformation = 0x8f,
}

struct Pack {
    pack_type: u8,
    track_number: u8,
    sequence: u8,
    character_position: u8,
    block_number: u8,
    double_byte: bool,

    payload: Box<[u8; 12]>,
}

impl Pack {
    pub fn into_bytes(self) -> Box<[u8; 18]> {
        let mut bytes = Box::new([
            self.pack_type,
            self.track_number,
            self.sequence,
            self.character_position & 0b1111
                | self.block_number << 4 & 0b111000
                | if self.double_byte { 0b10000000 } else { 0 },
            self.payload[0],
            self.payload[1],
            self.payload[2],
            self.payload[3],
            self.payload[4],
            self.payload[5],
            self.payload[6],
            self.payload[7],
            self.payload[8],
            self.payload[9],
            self.payload[10],
            self.payload[11],
            0,
            0,
        ]);

        let crc = crc::Crc::<u16>::new(&CD_TEXT_CRC_ALG);
        let (payload, crc_bytes) = bytes.split_at_mut(16);
        crc_bytes.copy_from_slice(&crc.checksum(&payload).to_be_bytes());

        bytes
    }
}

impl CdText {
    pub fn new() -> Self {
        CdText {
            title_data: Vec::new(),
            performer_data: Vec::new(),
            songwriter_data: Vec::new(),
            composer_data: Vec::new(),
            arranger_data: Vec::new(),
        }
    }

    pub fn append_track(&mut self, track_data: TrackData) {
        self.title_data.push(track_data.title);
        self.performer_data.push(track_data.performers);
        self.songwriter_data.push(track_data.songwriters);
        self.composer_data.push(track_data.composers);
        self.arranger_data.push(track_data.arrangers);
    }
    
    pub fn replace_track(&mut self, track_data: TrackData, track: usize) {
        self.title_data[track] = track_data.title;
        self.performer_data[track] = track_data.performers;
        self.songwriter_data[track] = track_data.songwriters;
        self.composer_data[track] = track_data.composers;
        self.arranger_data[track] = track_data.arrangers;
    }

    pub fn into_bytes(self) -> Vec<u8> {
        let mut seq = 0;
        let mut packs = Vec::new();
        packs.append(&mut Self::generate_packs(
            PackType::Title,
            |track| self.title_data.get(track as usize).cloned().flatten(),
            &mut seq,
        ));
        packs.append(&mut Self::generate_packs(
            PackType::Performers,
            |track| self.performer_data.get(track as usize).cloned().flatten(),
            &mut seq,
        ));

        let count_packs = |idx: usize| {
            packs
                .iter()
                .filter(|pack| pack.pack_type == (0x80 + idx) as u8)
                .count() as u8
        };

        let footer_pack_1 = Pack {
            pack_type: PackType::BlockSizeInformation as u8,
            track_number: 0,
            sequence: seq,
            character_position: 0,
            block_number: 0,
            double_byte: false,
            payload: Box::new([
                0x0,                             // Character code (ISO-8859-1)
                0x1,                             // Number of first track
                self.title_data.len() as u8 - 1, // Number of last track,
                0,                               // Copyright off
                count_packs(0),
                count_packs(1),
                count_packs(2),
                count_packs(3),
                count_packs(4),
                count_packs(5),
                count_packs(6),
                count_packs(7),
            ]),
        };
        seq += 1;
        let footer_pack_2 = Pack {
            pack_type: PackType::BlockSizeInformation as u8,
            track_number: 1,
            sequence: seq,
            character_position: 0,
            block_number: 0,
            double_byte: false,
            payload: Box::new([
                count_packs(8),
                count_packs(9),
                count_packs(10),
                count_packs(11),
                count_packs(12),
                count_packs(13),
                count_packs(14),
                3,       // # of BlockSizeInformation packs
                seq + 1, // Highest seq number
                0,
                0,
                0,
            ]),
        };
        seq += 1;
        let footer_pack_3 = Pack {
            pack_type: PackType::BlockSizeInformation as u8,
            track_number: 2,
            sequence: seq,
            character_position: 0,
            block_number: 0,
            double_byte: false,
            payload: Box::new([
                0, 0, 0, 0, 0x09, // Language code (English)
                0, 0, 0, 0, 0, 0, 0,
            ]),
        };
        packs.push(footer_pack_1);
        packs.push(footer_pack_2);
        packs.push(footer_pack_3);

        packs
            .into_iter()
            .flat_map(|pack| pack.into_bytes().into_iter())
            .collect::<Vec<u8>>()
    }

    fn generate_packs(
        pack_type: PackType,
        mut data_for_track: impl FnMut(u8) -> Option<String>,
        seq: &mut u8,
    ) -> Vec<Pack> {
        let mut packs = Vec::new();

        let mut track = 0;
        let mut pack_generator = PackGenerator::new(pack_type, 0);
        while let Some(data) = data_for_track(track) {
            let mut bytes = encoding_rs::WINDOWS_1252.encode(&data).0.to_vec();
            bytes.push(0);
            loop {
                let result = pack_generator.push_data(track, seq, &bytes);
                match result {
                    GenerationResult::Continue => break,
                    GenerationResult::Push(pack, read) => {
                        packs.push(pack);
                        bytes.drain(..read);
                    }
                }
            }
            track += 1;
        }

        if let Some(pack) = pack_generator.finalise(seq) {
            packs.push(pack);
        }

        packs
    }
}

struct PackGenerator {
    pack_type: PackType,
    pack_data: Vec<u8>,
    remaining: usize,
    character_position: u8,
    block_number: u8,

    pack_track: u8,
    pack_character_position: u8,
}

enum GenerationResult {
    Continue,
    Push(Pack, usize),
}

impl PackGenerator {
    pub fn new(pack_type: PackType, block_number: u8) -> Self {
        PackGenerator {
            pack_type,
            pack_data: {
                let mut data = Vec::new();
                data.reserve_exact(12);
                data
            },
            remaining: 12,
            character_position: 0,
            block_number,
            pack_track: 0,
            pack_character_position: 0,
        }
    }

    pub fn push_data(&mut self, track: u8, seq: &mut u8, data: &[u8]) -> GenerationResult {
        if self.remaining == 12 {
            self.pack_track = track;
            self.pack_character_position = self.character_position;
        }

        if data.len() >= self.remaining {
            self.pack_data.extend_from_slice(&data[..self.remaining]);
            let retval = GenerationResult::Push(
                Pack {
                    pack_type: self.pack_type as u8,
                    track_number: self.pack_track,
                    sequence: *seq,
                    character_position: self.pack_character_position,
                    block_number: self.block_number,
                    double_byte: false,
                    payload: std::mem::take(&mut self.pack_data)
                        .into_boxed_slice()
                        .try_into()
                        .unwrap(),
                },
                self.remaining,
            );
            *seq = seq.overflowing_add(1).0;
            self.pack_data.reserve_exact(12);
            if self.remaining == 12 && self.pack_track == track && self.character_position > 0 {
                // Current text started before the previous pack
                self.character_position = 15;
            } else if data.len() == self.remaining {
                self.character_position = 0;
            } else {
                self.character_position = self.remaining as u8;
            }
            self.remaining = 12;
            retval
        } else {
            self.pack_data.extend_from_slice(data);
            self.remaining -= data.len();
            self.character_position = self.remaining as u8;
            GenerationResult::Continue
        }
    }

    pub fn finalise(mut self, seq: &mut u8) -> Option<Pack> {
        if self.remaining == 12 {
            None
        } else {
            self.pack_data.resize(12, 0);
            let pack = Pack {
                pack_type: self.pack_type as u8,
                track_number: self.pack_track,
                sequence: *seq,
                character_position: self.pack_character_position,
                block_number: self.block_number,
                double_byte: false,
                payload: std::mem::take(&mut self.pack_data)
                    .into_boxed_slice()
                    .try_into()
                    .unwrap(),
            };
            *seq = seq.overflowing_add(1).0;
            Some(pack)
        }
    }
}
