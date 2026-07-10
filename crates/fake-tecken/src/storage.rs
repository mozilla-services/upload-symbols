use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct SymbolsStorage {
    uploaded_files: HashSet<String>,
    pending_uploads: HashMap<u32, String>,
    uploads: u32,
    resumable_uploads: u32,
}

impl SymbolsStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_upload(&mut self) -> u32 {
        self.uploads += 1;
        self.uploads
    }

    pub fn initiate_file_upload(&mut self, key: String) -> u32 {
        self.resumable_uploads += 1;
        self.pending_uploads.insert(self.resumable_uploads, key);
        self.resumable_uploads
    }

    pub fn complete_file_upload(&mut self, id: u32) -> Option<String> {
        let key = self.pending_uploads.remove(&id)?;
        self.upload_file(key.clone());
        Some(key)
    }

    pub fn upload_file(&mut self, key: String) {
        self.uploaded_files.insert(key);
    }

    pub fn uploaded_files(&self) -> &HashSet<String> {
        &self.uploaded_files
    }

    pub fn pending_uploads(&self) -> &HashMap<u32, String> {
        &self.pending_uploads
    }
}
