use anyhow::{bail, Result};
use arrow::datatypes::SchemaRef;
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use std::fs::File;
use std::io::{Seek, Write};
use std::path::Path;
use std::sync::Arc;

pub trait RowSink {
    fn write_batch(&mut self, batch: RecordBatch) -> Result<()>;

    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

pub struct ParquetSink<W: Write + Seek + Send> {
    writer: Option<ArrowWriter<W>>,
}

impl ParquetSink<File> {
    pub fn try_new(path: impl AsRef<Path>, schema: SchemaRef) -> Result<Self> {
        let file = File::create(path)?;
        Self::from_writer(file, schema, None)
    }
}

impl<W: Write + Seek + Send> ParquetSink<W> {
    pub fn from_writer(
        writer: W,
        schema: SchemaRef,
        props: Option<WriterProperties>,
    ) -> Result<Self> {
        let writer = ArrowWriter::try_new(writer, Arc::clone(&schema), props)?;
        Ok(Self {
            writer: Some(writer),
        })
    }
}

impl<W: Write + Seek + Send> RowSink for ParquetSink<W> {
    fn write_batch(&mut self, batch: RecordBatch) -> Result<()> {
        let Some(writer) = self.writer.as_mut() else {
            bail!("parquet sink is closed");
        };
        writer.write(&batch)?;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if let Some(writer) = self.writer.take() {
            let _ = writer.close()?;
        }
        Ok(())
    }
}
