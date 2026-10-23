use crate::{tds::collation::Collation, TypeInfo};
use bytes::buf::UninitSlice;
use bytes::{BufMut, BytesMut};
use std::borrow::{Borrow, BorrowMut};
use std::ops::{Deref, DerefMut};

pub(crate) struct BytesMutWithTypeInfo<'a> {
    bytes: &'a mut BytesMut,
    type_info: Option<&'a TypeInfo>,
    collation: Option<Collation>,
}

impl<'a> BytesMutWithTypeInfo<'a> {
    pub fn new(bytes: &'a mut BytesMut) -> Self {
        BytesMutWithTypeInfo {
            bytes,
            type_info: None,
            collation: None,
        }
    }

    pub fn with_type_info(mut self, type_info: &'a TypeInfo) -> Self {
        self.type_info = Some(type_info);
        self
    }

    /// The session's collation, which a `varchar` parameter is sent in. A
    /// column's own collation is in its type info instead.
    pub fn with_collation(mut self, collation: Option<Collation>) -> Self {
        self.collation = collation;
        self
    }

    pub fn type_info(&self) -> Option<&'a TypeInfo> {
        self.type_info
    }

    pub fn collation(&self) -> Option<Collation> {
        self.collation
    }
}

unsafe impl<'a> BufMut for BytesMutWithTypeInfo<'a> {
    fn remaining_mut(&self) -> usize {
        self.bytes.remaining_mut()
    }

    unsafe fn advance_mut(&mut self, cnt: usize) {
        self.bytes.advance_mut(cnt)
    }

    fn chunk_mut(&mut self) -> &mut UninitSlice {
        self.bytes.chunk_mut()
    }
}

impl<'a> Borrow<[u8]> for BytesMutWithTypeInfo<'a> {
    fn borrow(&self) -> &[u8] {
        self.bytes.deref()
    }
}

impl<'a> BorrowMut<[u8]> for BytesMutWithTypeInfo<'a> {
    fn borrow_mut(&mut self) -> &mut [u8] {
        self.bytes.borrow_mut()
    }
}

impl<'a> Deref for BytesMutWithTypeInfo<'a> {
    type Target = BytesMut;

    fn deref(&self) -> &Self::Target {
        self.bytes
    }
}

impl<'a> DerefMut for BytesMutWithTypeInfo<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.bytes
    }
}
