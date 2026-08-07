use crate::downloader::flv_parser::{
    AACPacketType, AVCPacketType, CodecId, FrameType, ScriptData, SoundFormat, SoundRate,
    SoundSize, SoundType, TagHeader,
};

use crate::downloader::util::LifecycleFile;
use byteorder::{BigEndian, WriteBytesExt};
use serde::Serialize;
use std::fs::File;
use std::io::{BufWriter, Write};

use tracing::{info, warn};

const FLV_HEADER: [u8; 9] = [
    0x46, 0x4c, 0x56, 0x01, 0x05, 0x00, 0x00, 0x00, 0x09,
];

pub struct FlvFile<'a> {
    pub buf_writer: BufWriter<File>,
    pub file: LifecycleFile<'a>,
}

impl<'a> FlvFile<'a> {
    pub fn new(mut file: LifecycleFile<'a>) -> std::io::Result<Self> {
        let path = file.create()?;
        Ok(Self {
            buf_writer: Self::create(path)?,
            file,
        })
    }

    /// Finalize the current FLV before exposing it to LifecycleFile's completion hook.
    /// This makes a rotation event a durable-file boundary rather than merely a pathname change.
    pub fn create_new(&mut self) -> std::io::Result<()> {
        self.sync_current()?;
        self.file.rename();
        let path = self.file.create()?;
        self.buf_writer = Self::create(path)?;
        Ok(())
    }

    fn sync_current(&mut self) -> std::io::Result<()> {
        self.buf_writer.flush()?;
        self.buf_writer.get_ref().sync_all()
    }

    fn create<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<BufWriter<File>> {
        let path = path.as_ref();
        let out = match File::create(path) {
            Ok(o) => o,
            Err(e) => {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!("Unable to create flv file {}", path.display()),
                ));
            }
        };
        info!("create flv file {}", path.display());
        let mut buf_writer = BufWriter::new(out);
        buf_writer.write_all(&FLV_HEADER)?;
        Self::write_previous_tag_size(&mut buf_writer, 0)?;
        Ok(buf_writer)
    }

    pub fn write_tag(
        &mut self,
        tag_header: &TagHeader,
        body: &[u8],
        previous_tag_size: &[u8],
    ) -> std::io::Result<usize> {
        self.write_tag_header(tag_header)?;
        self.buf_writer.write_all(body)?;
        self.buf_writer.write(previous_tag_size)
    }

    pub fn write_tag_header(&mut self, tag_header: &TagHeader) -> std::io::Result<()> {
        self.buf_writer.write_u8(tag_header.tag_type as u8)?;
        self.buf_writer.write_u24::<BigEndian>(tag_header.data_size)?;
        self.buf_writer
            .write_u24::<BigEndian>(tag_header.timestamp & 0xffffff)?;
        let timestamp_ext = ((tag_header.timestamp >> 24) & 0xff) as u8;
        self.buf_writer.write_u8(timestamp_ext)?;
        self.buf_writer.write_u24::<BigEndian>(tag_header.stream_id)
    }

    pub fn write_previous_tag_size(
        writer: &mut impl Write,
        previous_tag_size: u32,
    ) -> std::io::Result<usize> {
        writer.write(&previous_tag_size.to_be_bytes())
    }
}

impl Drop for FlvFile<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.sync_current() {
            warn!("failed to sync FLV before final rename: {error}");
            return;
        }
        self.file.rename()
    }
}

#[derive(Debug, PartialEq, Serialize)]
pub struct FlvTag<'a> {
    pub header: TagHeader,
    pub data: TagDataHeader<'a>,
}

impl FlvTag<'_> {}

#[derive(Debug, PartialEq, Serialize)]
pub enum TagDataHeader<'a> {
    Audio {
        sound_format: SoundFormat,
        sound_rate: SoundRate,
        sound_size: SoundSize,
        sound_type: SoundType,
        packet_type: Option<AACPacketType>,
    },
    Video {
        frame_type: FrameType,
        codec_id: CodecId,
        packet_type: Option<AVCPacketType>,
        composition_time: Option<i32>,
    },
    Script(ScriptData<'a>),
}

pub fn to_json<T: ?Sized + Serialize>(mut writer: impl Write, t: &T) -> std::io::Result<usize> {
    serde_json::to_writer(&mut writer, t)?;
    writer.write("\n".as_ref())
}
