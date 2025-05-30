use std::{sync::Mutex, time::Instant};

const MAX_READER_MERGE_GAP: u64 = 16 * 1024 * 1024; // 16MB

#[derive(Debug, Clone)]
pub struct Reader {
    pub position: u64,
    pub last_read: Instant,
    pub bytes_read: u64,
}

impl Reader {
    pub fn new(offset: u64, size: u64) -> Self {
        Self {
            position: offset + size,
            last_read: Instant::now(),
            bytes_read: size,
        }
    }

    pub fn matches(&self, offset: u64) -> bool {
        let gap = self.position.abs_diff(offset);
        if gap > MAX_READER_MERGE_GAP {
            false
        } else {
            true
        }
    }

    pub fn update(&mut self, offset: u64, size: u64) {
        self.position = offset + size;
        self.bytes_read += size;
        self.last_read = Instant::now();
    }
}

#[derive(Debug)]
pub struct Readers {
    readers: Mutex<Vec<Reader>>,
}

impl Readers {
    pub fn new() -> Self {
        Self {
            readers: Mutex::new(Vec::new()),
        }
    }

    pub fn get_reader(&self, offset: u64, size: u64) -> Reader {
        let mut readers = self.readers.lock().unwrap();

        for reader in readers.iter_mut() {
            if reader.matches(offset) {
                reader.update(offset, size);
                return reader.clone();
            }
        }

        let new_reader = Reader::new(offset, size);
        readers.push(new_reader.clone());
        new_reader
    }
}
