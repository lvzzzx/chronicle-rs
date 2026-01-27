use anyhow::{bail, Result};
use arrow::array::{ArrayRef, BooleanBuilder, Float64Builder, Int64Builder, UInt64Builder};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;

use crate::stream::replay::{BookEvent, L2Book};

#[derive(Debug, Clone)]
pub struct ColumnSpec {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
}

impl ColumnSpec {
    pub fn new(name: impl Into<String>, data_type: DataType) -> Self {
        Self {
            name: name.into(),
            data_type,
            nullable: false,
        }
    }

    pub fn nullable(mut self, nullable: bool) -> Self {
        self.nullable = nullable;
        self
    }
}

pub trait Feature {
    fn schema(&self) -> Vec<ColumnSpec>;

    fn on_event(&mut self, book: &L2Book, event: &BookEvent<'_>, out: &mut RowWriter<'_>);
}

pub trait FeatureSet {
    fn schema(&self) -> SchemaRef;

    fn calculate(&mut self, book: &L2Book, event: &BookEvent<'_>, out: &mut RowWriter<'_>);

    fn should_emit(&mut self, _event: &BookEvent<'_>) -> bool {
        true
    }
}

pub trait Trigger {
    fn should_emit(&mut self, event: &BookEvent<'_>) -> bool;
}

#[derive(Debug, Default)]
pub struct AlwaysEmit;

impl Trigger for AlwaysEmit {
    fn should_emit(&mut self, _event: &BookEvent<'_>) -> bool {
        true
    }
}

pub struct FeatureGraph {
    features: Vec<Box<dyn Feature>>,
    offsets: Vec<usize>,
    schema: SchemaRef,
    trigger: Box<dyn Trigger>,
}

impl FeatureGraph {
    pub fn new(features: Vec<Box<dyn Feature>>) -> Result<Self> {
        let mut fields = Vec::new();
        let mut offsets = Vec::with_capacity(features.len());
        let mut col_idx = 0usize;

        for feature in &features {
            offsets.push(col_idx);
            for col in feature.schema() {
                let field = Field::new(&col.name, col.data_type, col.nullable);
                fields.push(field);
                col_idx = col_idx.saturating_add(1);
            }
        }

        if fields.is_empty() {
            bail!("feature graph schema is empty");
        }

        let schema = Arc::new(Schema::new(fields));

        Ok(Self {
            features,
            offsets,
            schema,
            trigger: Box::new(AlwaysEmit::default()),
        })
    }

    pub fn with_trigger(mut self, trigger: impl Trigger + 'static) -> Self {
        self.trigger = Box::new(trigger);
        self
    }
}

impl FeatureSet for FeatureGraph {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn calculate(&mut self, book: &L2Book, event: &BookEvent<'_>, out: &mut RowWriter<'_>) {
        let emit = out.emit();
        let buffer = out.buffer();

        for (feature, offset) in self.features.iter_mut().zip(self.offsets.iter().copied()) {
            let mut writer = RowWriter::with_offset(buffer, emit, offset);
            feature.on_event(book, event, &mut writer);
        }
    }

    fn should_emit(&mut self, event: &BookEvent<'_>) -> bool {
        self.trigger.should_emit(event)
    }
}

pub struct RowBuffer {
    schema: SchemaRef,
    builders: Vec<ColumnBuilder>,
    row_count: usize,
    batch_size: usize,
}

impl RowBuffer {
    pub fn new(schema: SchemaRef, batch_size: usize) -> Result<Self> {
        if batch_size == 0 {
            bail!("batch_size must be > 0");
        }
        let mut builders = Vec::with_capacity(schema.fields().len());
        for field in schema.fields() {
            builders.push(ColumnBuilder::new(field.data_type(), batch_size)?);
        }
        Ok(Self {
            schema,
            builders,
            row_count: 0,
            batch_size,
        })
    }

    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn is_empty(&self) -> bool {
        self.row_count == 0
    }

    pub fn should_flush(&self) -> bool {
        self.row_count >= self.batch_size
    }

    pub fn commit_row(&mut self) {
        self.row_count = self.row_count.saturating_add(1);
    }

    pub fn append_f64(&mut self, col_idx: usize, value: f64) {
        match self.builders.get_mut(col_idx) {
            Some(ColumnBuilder::F64(builder)) => builder.append_value(value),
            Some(other) => panic!("column {col_idx} expects {:?}", other.data_type()),
            None => panic!("column {col_idx} out of range"),
        }
    }

    pub fn append_i64(&mut self, col_idx: usize, value: i64) {
        match self.builders.get_mut(col_idx) {
            Some(ColumnBuilder::I64(builder)) => builder.append_value(value),
            Some(other) => panic!("column {col_idx} expects {:?}", other.data_type()),
            None => panic!("column {col_idx} out of range"),
        }
    }

    pub fn append_u64(&mut self, col_idx: usize, value: u64) {
        match self.builders.get_mut(col_idx) {
            Some(ColumnBuilder::U64(builder)) => builder.append_value(value),
            Some(other) => panic!("column {col_idx} expects {:?}", other.data_type()),
            None => panic!("column {col_idx} out of range"),
        }
    }

    pub fn append_bool(&mut self, col_idx: usize, value: bool) {
        match self.builders.get_mut(col_idx) {
            Some(ColumnBuilder::Bool(builder)) => builder.append_value(value),
            Some(other) => panic!("column {col_idx} expects {:?}", other.data_type()),
            None => panic!("column {col_idx} out of range"),
        }
    }

    pub fn flush(&mut self) -> Result<RecordBatch> {
        if self.row_count == 0 {
            bail!("attempted to flush an empty RowBuffer");
        }
        let arrays: Vec<ArrayRef> = self.builders.iter_mut().map(|b| b.finish()).collect();
        let batch = RecordBatch::try_new(self.schema.clone(), arrays)?;
        self.row_count = 0;
        Ok(batch)
    }
}

pub struct RowWriter<'a> {
    buffer: &'a mut RowBuffer,
    emit: bool,
    base_col: usize,
}

impl<'a> RowWriter<'a> {
    pub fn new(buffer: &'a mut RowBuffer, emit: bool) -> Self {
        Self {
            buffer,
            emit,
            base_col: 0,
        }
    }

    pub fn with_offset(buffer: &'a mut RowBuffer, emit: bool, base_col: usize) -> Self {
        Self {
            buffer,
            emit,
            base_col,
        }
    }

    pub fn emit(&self) -> bool {
        self.emit
    }

    pub fn buffer(&mut self) -> &mut RowBuffer {
        self.buffer
    }

    pub fn append_f64(&mut self, col_idx: usize, value: f64) {
        if self.emit {
            self.buffer.append_f64(self.base_col + col_idx, value);
        }
    }

    pub fn append_i64(&mut self, col_idx: usize, value: i64) {
        if self.emit {
            self.buffer.append_i64(self.base_col + col_idx, value);
        }
    }

    pub fn append_u64(&mut self, col_idx: usize, value: u64) {
        if self.emit {
            self.buffer.append_u64(self.base_col + col_idx, value);
        }
    }

    pub fn append_bool(&mut self, col_idx: usize, value: bool) {
        if self.emit {
            self.buffer.append_bool(self.base_col + col_idx, value);
        }
    }
}

enum ColumnBuilder {
    F64(Float64Builder),
    I64(Int64Builder),
    U64(UInt64Builder),
    Bool(BooleanBuilder),
}

impl ColumnBuilder {
    fn new(data_type: &DataType, batch_size: usize) -> Result<Self> {
        match data_type {
            DataType::Float64 => Ok(Self::F64(Float64Builder::with_capacity(batch_size))),
            DataType::Int64 => Ok(Self::I64(Int64Builder::with_capacity(batch_size))),
            DataType::UInt64 => Ok(Self::U64(UInt64Builder::with_capacity(batch_size))),
            DataType::Boolean => Ok(Self::Bool(BooleanBuilder::with_capacity(batch_size))),
            other => bail!("unsupported column type: {other:?}"),
        }
    }

    fn data_type(&self) -> DataType {
        match self {
            ColumnBuilder::F64(_) => DataType::Float64,
            ColumnBuilder::I64(_) => DataType::Int64,
            ColumnBuilder::U64(_) => DataType::UInt64,
            ColumnBuilder::Bool(_) => DataType::Boolean,
        }
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
            ColumnBuilder::F64(builder) => Arc::new(builder.finish()),
            ColumnBuilder::I64(builder) => Arc::new(builder.finish()),
            ColumnBuilder::U64(builder) => Arc::new(builder.finish()),
            ColumnBuilder::Bool(builder) => Arc::new(builder.finish()),
        }
    }
}
