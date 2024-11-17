use super::constants::*;
use super::fm_chunk::FMChunk;
use crate::formats::readers::{get_file_sizes_and_readers, AsyncReader};
use crate::lava::error::LavaError;

use crate::lava::substring::wavelet_tree::{construct_wavelet_tree, write_wavelet_tree_to_disk};
use arrow::array::{make_array, Array, ArrayData, LargeStringArray, UInt64Array};
use bincode;
use bytes;
use divsufsort::sort_in_place;
use itertools::Itertools;
use serde_json;

use rayon::prelude::*;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::fs::File;
use std::io::Read;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use tokenizers::parallelism::MaybeParallelIterator;
use tokenizers::tokenizer::Tokenizer; // You'll need the `byteorder` crate
use tokio::task::JoinSet;
use zstd::stream::encode_all;
use zstd::stream::read::Decoder;

pub async fn _build_lava_substring_char_wavelet(
    output_file_name: String,
    texts: Vec<(u64, String)>,
    char_skip_factor: u32,
) -> Result<Vec<(usize, usize)>, LavaError> {
    let named_encodings = texts
        .into_iter()
        .map(|(uid, text)| {
            let lower: String = text.chars().flat_map(|c| c.to_lowercase()).collect();
            let result: Vec<u8> = if char_skip_factor == 1 {
                lower
                    .chars()
                    .filter(|id| !SKIP.chars().contains(id))
                    .map(|c| c as u8)
                    .collect()
            } else {
                lower
                    .chars()
                    .filter(|id| !SKIP.chars().contains(id))
                    .enumerate()
                    .filter(|&(index, _)| index % char_skip_factor as usize == 1)
                    .map(|(_, c)| c as u8)
                    .collect()
            };
            (vec![uid; result.len()], result)
        })
        .collect::<Vec<(Vec<u64>, Vec<u8>)>>();

    let uids: Vec<u64> = named_encodings
        .iter()
        .map(|(uid, _)| uid)
        .flatten()
        .cloned()
        .collect::<Vec<u64>>();
    let encodings: Vec<u8> = named_encodings
        .into_iter()
        .map(|(_, text)| text)
        .flatten()
        .collect::<Vec<u8>>();

    let mut sa: Vec<i32> = (0..encodings.len() as i32).collect();

    sort_in_place(&encodings, &mut sa);

    let mut idx: Vec<u64> = Vec::with_capacity(encodings.len());
    let mut bwt: Vec<u8> = Vec::with_capacity(encodings.len());
    let mut total_counts: Vec<usize> = vec![0; 256];
    for i in 0..sa.len() {
        let char = if sa[i] == 0 {
            encodings[encodings.len() - 1]
        } else {
            encodings[(sa[i] - 1) as usize]
        };
        bwt.push(char);
        total_counts[char as usize] += 1;
        if sa[i] == 0 {
            idx.push(uids[uids.len() - 1]);
        } else {
            idx.push(uids[(sa[i] - 1) as usize]);
        }
    }

    let mut cumulative_counts = vec![0; 256];
    cumulative_counts[0] = 0;
    for i in 1..256 {
        cumulative_counts[i] = cumulative_counts[i - 1] + total_counts[i - 1];
    }

    let wavelet_tree = construct_wavelet_tree(&bwt);

    let mut file = File::create(output_file_name)?;

    let (offsets, level_offsets) = write_wavelet_tree_to_disk(&wavelet_tree, &mut file).unwrap();

    // print out total file size so far
    println!("total file size: {}", file.seek(SeekFrom::Current(0))?);

    let mut posting_list_offsets: Vec<usize> = vec![file.seek(SeekFrom::Current(0))? as usize];

    for i in (0..idx.len()).step_by(FM_CHUNK_TOKS) {
        let slice = &idx[i..std::cmp::min(idx.len(), i + FM_CHUNK_TOKS)];
        let serialized_slice = bincode::serialize(slice)?;
        let compressed_slice = encode_all(&serialized_slice[..], 0).expect("Compression failed");
        file.write_all(&compressed_slice)?;
        posting_list_offsets.push(file.seek(SeekFrom::Current(0))? as usize);
    }

    let metadata: (Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>, usize) = (
        offsets,
        level_offsets,
        posting_list_offsets,
        cumulative_counts,
        bwt.len(),
    );

    let cache_start = file.seek(SeekFrom::Current(0))? as usize;

    let serialized_metadata = bincode::serialize(&metadata)?;
    let compressed_metadata = encode_all(&serialized_metadata[..], 0).expect("Compression failed");
    file.write_all(&compressed_metadata)?;
    file.write_all(&cache_start.to_le_bytes())?;

    let cache_end = file.seek(SeekFrom::Current(0))? as usize;

    Ok(vec![(cache_start, cache_end)])
}

pub async fn _build_lava_substring_char(
    output_file_name: String,
    texts: Vec<(u64, String)>,
    char_skip_factor: u32,
) -> Result<Vec<(usize, usize)>, LavaError> {
    let named_encodings = texts
        .into_iter()
        .map(|(uid, text)| {
            let lower: String = text.chars().flat_map(|c| c.to_lowercase()).collect();
            let result: Vec<u8> = if char_skip_factor == 1 {
                lower
                    .chars()
                    .filter(|id| !SKIP.chars().contains(id))
                    .map(|c| c as u8)
                    .collect()
            } else {
                lower
                    .chars()
                    .filter(|id| !SKIP.chars().contains(id))
                    .enumerate()
                    .filter(|&(index, _)| index % char_skip_factor as usize == 1)
                    .map(|(_, c)| c as u8)
                    .collect()
            };
            (vec![uid; result.len()], result)
        })
        .collect::<Vec<(Vec<u64>, Vec<u8>)>>();

    let uids: Vec<u64> = named_encodings
        .iter()
        .map(|(uid, _)| uid)
        .flatten()
        .cloned()
        .collect::<Vec<u64>>();
    let encodings: Vec<u8> = named_encodings
        .into_iter()
        .map(|(_, text)| text)
        .flatten()
        .collect::<Vec<u8>>();

    let mut sa: Vec<i32> = (0..encodings.len() as i32).collect();

    sort_in_place(&encodings, &mut sa);

    let mut idx: Vec<u64> = Vec::with_capacity(encodings.len());
    let mut bwt: Vec<u8> = Vec::with_capacity(encodings.len());
    for i in 0..sa.len() {
        if sa[i] == 0 {
            bwt.push(encodings[encodings.len() - 1]);
            idx.push(uids[uids.len() - 1]);
        } else {
            bwt.push(encodings[(sa[i] - 1) as usize]);
            idx.push(uids[(sa[i] - 1) as usize]);
        }
    }

    let mut file = File::create(output_file_name)?;

    let mut fm_chunk_offsets: Vec<usize> = vec![file.seek(SeekFrom::Current(0))? as usize];

    let mut current_chunk: Vec<u8> = vec![];
    let mut current_chunk_counts: HashMap<u8, u64> = HashMap::new();
    let mut next_chunk_counts: HashMap<u8, u64> = HashMap::new();

    for i in 0..bwt.len() {
        let current_tok = bwt[i];
        next_chunk_counts
            .entry(current_tok)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        current_chunk.push(current_tok);

        if ((i + 1) % FM_CHUNK_TOKS == 0) || i == bwt.len() - 1 {
            let serialized_counts = bincode::serialize(&current_chunk_counts)?;
            let compressed_counts =
                encode_all(&serialized_counts[..], 10).expect("Compression failed");
            println!("chunk size: {}", compressed_counts.len());
            file.write_all(&(compressed_counts.len() as u64).to_le_bytes())?;
            file.write_all(&compressed_counts)?;
            let serialized_chunk = bincode::serialize(&current_chunk)?;
            let compressed_chunk =
                encode_all(&serialized_chunk[..], 10).expect("Compression failed");
            file.write_all(&compressed_chunk)?;
            fm_chunk_offsets.push(file.seek(SeekFrom::Current(0))? as usize);
            current_chunk_counts = next_chunk_counts.clone();
            current_chunk = vec![];
        }
    }
    // print out total file size so far
    println!("total file size: {}", file.seek(SeekFrom::Current(0))?);

    let mut cumulative_counts: Vec<u64> = vec![0];
    for i in 0..256 {
        cumulative_counts
            .push(cumulative_counts[i] + *current_chunk_counts.get(&(i as u8)).unwrap_or(&0));
    }

    let mut posting_list_offsets: Vec<usize> = vec![file.seek(SeekFrom::Current(0))? as usize];

    for i in (0..idx.len()).step_by(FM_CHUNK_TOKS) {
        let slice = &idx[i..std::cmp::min(idx.len(), i + FM_CHUNK_TOKS)];
        let serialized_slice = bincode::serialize(slice)?;
        let compressed_slice = encode_all(&serialized_slice[..], 0).expect("Compression failed");
        file.write_all(&compressed_slice)?;
        posting_list_offsets.push(file.seek(SeekFrom::Current(0))? as usize);
    }

    let cache_start = file.seek(SeekFrom::Current(0))? as usize;

    let fm_chunk_offsets_offset = file.seek(SeekFrom::Current(0))? as usize;
    let serialized_fm_chunk_offsets = bincode::serialize(&fm_chunk_offsets)?;
    let compressed_fm_chunk_offsets =
        encode_all(&serialized_fm_chunk_offsets[..], 0).expect("Compression failed");
    file.write_all(&compressed_fm_chunk_offsets)?;

    let posting_list_offsets_offset = file.seek(SeekFrom::Current(0))? as usize;
    let serialized_posting_list_offsets = bincode::serialize(&posting_list_offsets)?;
    let compressed_posting_list_offsets =
        encode_all(&serialized_posting_list_offsets[..], 0).expect("Compression failed");
    file.write_all(&compressed_posting_list_offsets)?;

    let total_counts_offset = file.seek(SeekFrom::Current(0))? as usize;
    let serialized_total_counts = bincode::serialize(&cumulative_counts)?;
    let compressed_total_counts: Vec<u8> =
        encode_all(&serialized_total_counts[..], 0).expect("Compression failed");
    file.write_all(&compressed_total_counts)?;

    file.write_all(&(fm_chunk_offsets_offset as u64).to_le_bytes())?;
    file.write_all(&(posting_list_offsets_offset as u64).to_le_bytes())?;
    file.write_all(&(total_counts_offset as u64).to_le_bytes())?;
    file.write_all(&(bwt.len() as u64).to_le_bytes())?;

    let cache_end = file.seek(SeekFrom::Current(0))? as usize;

    Ok(vec![(cache_start, cache_end)])
}

#[tokio::main]
pub async fn build_lava_substring_char(
    output_file_name: String,
    array: ArrayData,
    uid: ArrayData,
    char_skip_factor: Option<u32>,
) -> Result<Vec<(usize, usize)>, LavaError> {
    let array = make_array(array);
    // let uid = make_array(ArrayData::from_pyarrow(uid)?);
    let uid = make_array(uid);

    let char_skip_factor = char_skip_factor.unwrap_or(1);

    let array: &arrow_array::GenericByteArray<arrow_array::types::GenericStringType<i64>> = array
        .as_any()
        .downcast_ref::<LargeStringArray>()
        .ok_or(LavaError::Parse(
            "Expects string array as first argument".to_string(),
        ))?;

    let uid = uid
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or(LavaError::Parse(
            "Expects uint64 array as second argument".to_string(),
        ))?;

    if array.len() != uid.len() {
        return Err(LavaError::Parse(
            "The length of the array and the uid array must be the same".to_string(),
        ));
    }

    let mut texts: Vec<(u64, String)> = Vec::with_capacity(array.len());
    for i in 0..array.len() {
        let text = array.value(i);
        texts.push((uid.value(i), text.to_string()));
    }

    println!("made it to this point");
    // _build_lava_substring_char(output_file_name, texts, char_skip_factor).await
    _build_lava_substring_char_wavelet(output_file_name, texts, char_skip_factor).await
}

#[tokio::main]
pub async fn build_lava_substring(
    output_file_name: String,
    array: ArrayData,
    uid: ArrayData,
    tokenizer_file: Option<String>,
    token_skip_factor: Option<u32>,
) -> Result<Vec<(usize, usize)>, LavaError> {
    let array = make_array(array);
    // let uid = make_array(ArrayData::from_pyarrow(uid)?);
    let uid = make_array(uid);

    let token_skip_factor = token_skip_factor.unwrap_or(1);

    let tokenizer = if let Some(tokenizer_file) = tokenizer_file {
        if !std::path::Path::new(&tokenizer_file).exists() {
            return Err(LavaError::Parse(
                "Tokenizer file does not exist".to_string(),
            ));
        }
        println!("Tokenizer file: {}", tokenizer_file);
        Tokenizer::from_file(tokenizer_file).unwrap()
    } else {
        Tokenizer::from_pretrained("bert-base-uncased", None).unwrap()
    };

    let serialized_tokenizer = serde_json::to_string(&tokenizer).unwrap();
    let compressed_tokenizer =
        encode_all(serialized_tokenizer.as_bytes(), 0).expect("Compression failed");

    let array: &arrow_array::GenericByteArray<arrow_array::types::GenericStringType<i64>> = array
        .as_any()
        .downcast_ref::<LargeStringArray>()
        .ok_or(LavaError::Parse(
            "Expects string array as first argument".to_string(),
        ))?;

    let uid = uid
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or(LavaError::Parse(
            "Expects uint64 array as second argument".to_string(),
        ))?;

    if array.len() != uid.len() {
        return Err(LavaError::Parse(
            "The length of the array and the uid array must be the same".to_string(),
        ));
    }

    let mut texts: Vec<(u64, &str)> = Vec::with_capacity(array.len());
    for i in 0..array.len() {
        let text = array.value(i);
        texts.push((uid.value(i), text));
    }

    let mut skip_tokens: HashSet<u32> = HashSet::new();
    for char in SKIP.chars() {
        let char_str = char.to_string();
        skip_tokens.extend(
            tokenizer
                .encode(char_str.clone(), false)
                .unwrap()
                .get_ids()
                .to_vec(),
        );
        skip_tokens.extend(
            tokenizer
                .encode(format!(" {}", char_str), false)
                .unwrap()
                .get_ids()
                .to_vec(),
        );
        skip_tokens.extend(
            tokenizer
                .encode(format!("{} ", char_str), false)
                .unwrap()
                .get_ids()
                .to_vec(),
        );
    }

    let named_encodings = texts
        .into_maybe_par_iter()
        .map(|(uid, text)| {
            // strip out things in skip in text

            let lower: String = text.chars().flat_map(|c| c.to_lowercase()).collect();
            let encoding = tokenizer.encode(lower, false).unwrap();
            let result: Vec<u32> = encoding
                .get_ids()
                .iter()
                .filter(|id| !skip_tokens.contains(id))
                .cloned()
                .collect();
            (vec![uid; result.len()], result)
        })
        .collect::<Vec<(Vec<u64>, Vec<u32>)>>();

    let uids: Vec<u64> = named_encodings
        .iter()
        .map(|(uid, _)| uid)
        .flatten()
        .cloned()
        .collect::<Vec<u64>>();
    let encodings: Vec<u32> = named_encodings
        .into_iter()
        .map(|(_, text)| text)
        .flatten()
        .collect::<Vec<u32>>();

    let mut suffices: Vec<Vec<u32>> = vec![];

    let (encodings, uids) = if token_skip_factor > 1 {
        let encodings: Vec<u32> = encodings
            .into_iter()
            .enumerate() // Enumerate to get the index and value
            .filter(|&(index, _)| index % token_skip_factor as usize == 1) // Keep only elements with odd indices (every second element)
            .map(|(_, value)| value) // Extract the value
            .collect(); // Collect into a vector

        let uids: Vec<u64> = uids
            .into_iter()
            .enumerate() // Enumerate to get the index and value
            .filter(|&(index, _)| index % token_skip_factor as usize == 1) // Keep only elements with odd indices (every second element)
            .map(|(_, value)| value) // Extract the value
            .collect();
        (encodings, uids)
    } else {
        (encodings, uids)
    };

    for i in 10..encodings.len() {
        suffices.push(encodings[i - 10..i].to_vec());
    }

    for i in encodings.len()..encodings.len() + 10 {
        let mut suffix = encodings[i - 10..encodings.len()].to_vec();
        suffix.append(&mut vec![0; i - encodings.len()]);
        suffices.push(suffix);
    }

    let mut sa: Vec<usize> = (0..suffices.len()).collect();

    sa.par_sort_by(|&a, &b| suffices[a].cmp(&suffices[b]));

    let mut idx: Vec<u64> = Vec::with_capacity(encodings.len());
    let mut bwt: Vec<u32> = Vec::with_capacity(encodings.len());
    for i in 0..sa.len() {
        if sa[i] == 0 {
            bwt.push(encodings[encodings.len() - 1]);
            idx.push(uids[uids.len() - 1]);
        } else {
            bwt.push(encodings[(sa[i] - 1) as usize]);
            idx.push(uids[(sa[i] - 1) as usize]);
        }
    }

    let mut file = File::create(output_file_name)?;
    file.write_all(&(compressed_tokenizer.len() as u64).to_le_bytes())?;
    file.write_all(&compressed_tokenizer)?;

    let mut fm_chunk_offsets: Vec<usize> = vec![file.seek(SeekFrom::Current(0))? as usize];

    let mut current_chunk: Vec<u32> = vec![];
    let mut current_chunk_counts: HashMap<u32, u64> = HashMap::new();
    let mut next_chunk_counts: HashMap<u32, u64> = HashMap::new();

    for i in 0..bwt.len() {
        let current_tok = bwt[i];
        next_chunk_counts
            .entry(current_tok)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        current_chunk.push(current_tok);

        if ((i + 1) % FM_CHUNK_TOKS == 0) || i == bwt.len() - 1 {
            let serialized_counts = bincode::serialize(&current_chunk_counts)?;
            let compressed_counts =
                encode_all(&serialized_counts[..], 10).expect("Compression failed");

            file.write_all(&(compressed_counts.len() as u64).to_le_bytes())?;
            file.write_all(&compressed_counts)?;
            let serialized_chunk = bincode::serialize(&current_chunk)?;
            let compressed_chunk =
                encode_all(&serialized_chunk[..], 10).expect("Compression failed");
            file.write_all(&compressed_chunk)?;

            fm_chunk_offsets.push(file.seek(SeekFrom::Current(0))? as usize);
            current_chunk_counts = next_chunk_counts.clone();
            current_chunk = vec![];
        }
    }
    // print out total file size so far
    println!("total file size: {}", file.seek(SeekFrom::Current(0))?);

    let mut cumulative_counts: Vec<u64> = vec![0];
    for i in 0..tokenizer.get_vocab_size(false) {
        cumulative_counts
            .push(cumulative_counts[i] + *current_chunk_counts.get(&(i as u32)).unwrap_or(&0));
    }

    let mut posting_list_offsets: Vec<usize> = vec![file.seek(SeekFrom::Current(0))? as usize];

    for i in (0..idx.len()).step_by(FM_CHUNK_TOKS) {
        let slice = &idx[i..std::cmp::min(idx.len(), i + FM_CHUNK_TOKS)];
        let serialized_slice = bincode::serialize(slice)?;
        let compressed_slice = encode_all(&serialized_slice[..], 0).expect("Compression failed");
        file.write_all(&compressed_slice)?;
        posting_list_offsets.push(file.seek(SeekFrom::Current(0))? as usize);
    }

    let cache_start = file.seek(SeekFrom::Current(0))? as usize;

    let fm_chunk_offsets_offset = file.seek(SeekFrom::Current(0))? as usize;
    let serialized_fm_chunk_offsets = bincode::serialize(&fm_chunk_offsets)?;
    let compressed_fm_chunk_offsets =
        encode_all(&serialized_fm_chunk_offsets[..], 0).expect("Compression failed");
    file.write_all(&compressed_fm_chunk_offsets)?;

    let posting_list_offsets_offset = file.seek(SeekFrom::Current(0))? as usize;
    let serialized_posting_list_offsets = bincode::serialize(&posting_list_offsets)?;
    let compressed_posting_list_offsets =
        encode_all(&serialized_posting_list_offsets[..], 0).expect("Compression failed");
    file.write_all(&compressed_posting_list_offsets)?;

    let total_counts_offset = file.seek(SeekFrom::Current(0))? as usize;
    let serialized_total_counts = bincode::serialize(&cumulative_counts)?;
    let compressed_total_counts: Vec<u8> =
        encode_all(&serialized_total_counts[..], 0).expect("Compression failed");
    file.write_all(&compressed_total_counts)?;

    file.write_all(&(fm_chunk_offsets_offset as u64).to_le_bytes())?;
    file.write_all(&(posting_list_offsets_offset as u64).to_le_bytes())?;
    file.write_all(&(total_counts_offset as u64).to_le_bytes())?;
    file.write_all(&(bwt.len() as u64).to_le_bytes())?;

    let cache_end = file.seek(SeekFrom::Current(0))? as usize;

    Ok(vec![(cache_start, cache_end)])
}

use num_traits::{AsPrimitive, PrimInt, Unsigned};
use serde::{Deserialize, Serialize};
use std::ops::Add;

async fn process_substring_query<T>(
    query: Vec<T>,
    n: u64,
    fm_chunk_offsets: &[u64],
    cumulative_counts: &[u64],
    posting_list_offsets: &[u64],
    reader: &mut AsyncReader,
    file_id: u64,
) -> Vec<(u64, u64)>
where
    T: PrimInt
        + Unsigned
        + Serialize
        + for<'de> Deserialize<'de>
        + Clone
        + Eq
        + std::hash::Hash
        + AsPrimitive<usize>
        + 'static,
    usize: AsPrimitive<T>,
{
    let mut res: Vec<(u64, u64)> = vec![];
    let mut start: usize = 0;
    let mut end: usize = n as usize;

    for i in (0..query.len()).rev() {
        let current_token = query[i];

        let start_byte = fm_chunk_offsets[start / FM_CHUNK_TOKS];
        let end_byte = fm_chunk_offsets[start / FM_CHUNK_TOKS + 1];
        let start_chunk = reader.read_range(start_byte, end_byte).await.unwrap();

        let start_byte = fm_chunk_offsets[end / FM_CHUNK_TOKS];
        let end_byte = fm_chunk_offsets[end / FM_CHUNK_TOKS + 1];
        let end_chunk = reader.read_range(start_byte, end_byte).await.unwrap();

        start = cumulative_counts[current_token.as_()] as usize
            + FMChunk::<T>::new(start_chunk)
                .unwrap()
                .search(current_token, start % FM_CHUNK_TOKS)
                .unwrap() as usize;
        end = cumulative_counts[current_token.as_()] as usize
            + FMChunk::<T>::new(end_chunk)
                .unwrap()
                .search(current_token, end % FM_CHUNK_TOKS)
                .unwrap() as usize;

        if start >= end {
            return res;
        }

        if end <= start + 2 {
            break;
        }
    }

    let start_offset = posting_list_offsets[start / FM_CHUNK_TOKS];
    let end_offset = posting_list_offsets[end / FM_CHUNK_TOKS + 1];
    let total_chunks = end / FM_CHUNK_TOKS - start / FM_CHUNK_TOKS + 1;

    let plist_chunks = reader.read_range(start_offset, end_offset).await.unwrap();

    let mut chunk_set = JoinSet::new();

    for i in 0..total_chunks {
        let this_start = posting_list_offsets[start / FM_CHUNK_TOKS + i];
        let this_end = posting_list_offsets[start / FM_CHUNK_TOKS + i + 1];
        let this_chunk = plist_chunks
            [(this_start - start_offset) as usize..(this_end - start_offset) as usize]
            .to_vec();

        chunk_set.spawn(async move {
            let mut decompressor = Decoder::new(&this_chunk[..]).unwrap();
            let mut serialized_plist_chunk: Vec<u8> = Vec::with_capacity(this_chunk.len());
            decompressor
                .read_to_end(&mut serialized_plist_chunk)
                .unwrap();
            let plist_chunk: Vec<u64> = bincode::deserialize(&serialized_plist_chunk).unwrap();

            let chunk_res: Vec<(u64, u64)> = if i == 0 {
                if total_chunks == 1 {
                    plist_chunk[start % FM_CHUNK_TOKS..end % FM_CHUNK_TOKS]
                        .iter()
                        .map(|&uid| (file_id, uid))
                        .collect()
                } else {
                    plist_chunk[start % FM_CHUNK_TOKS..]
                        .iter()
                        .map(|&uid| (file_id, uid))
                        .collect()
                }
            } else if i == total_chunks - 1 {
                plist_chunk[..end % FM_CHUNK_TOKS]
                    .iter()
                    .map(|&uid| (file_id, uid))
                    .collect()
            } else {
                plist_chunk.iter().map(|&uid| (file_id, uid)).collect()
            };

            chunk_res
        });
    }

    while let Some(chunk_res) = chunk_set.join_next().await {
        res.extend(chunk_res.unwrap());
    }

    res
}

use super::wavelet_tree::search_wavelet_tree_from_reader;
use crate::formats::readers::read_and_decompress;

async fn search_substring_wavelet_one_file(
    file_id: u64,
    mut reader: AsyncReader,
    file_size: usize,
    queries: Vec<Vec<u8>>,
) -> Result<Vec<(u64, u64)>, LavaError> {
    println!("{:?}", queries);

    let metadata_start = reader.read_usize_from_end(1).await?[0];

    let metadata: (Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>, usize) = read_and_decompress(
        &mut reader,
        metadata_start as u64,
        file_size as u64 - metadata_start - 8,
    )
    .await
    .unwrap();
    let (offsets, level_offsets, posting_list_offsets, cumulative_counts, n) = metadata;

    // let mut query_set = JoinSet::new();

    let mut res: Vec<(u64, u64)> = vec![];

    for query in queries {
        let mut reader = reader.clone();
        let (start, end) = search_wavelet_tree_from_reader(
            &mut reader,
            &query,
            n,
            &offsets,
            &level_offsets,
            &cumulative_counts,
        )
        .await?;

        println!("{} {}", start, end);

        if start == end {
            continue;
        }

        let start_offset = posting_list_offsets[start / FM_CHUNK_TOKS];
        let end_offset = posting_list_offsets[end / FM_CHUNK_TOKS + 1];
        let total_chunks = end / FM_CHUNK_TOKS - start / FM_CHUNK_TOKS + 1;

        let plist_chunks = reader
            .read_range(start_offset as u64, end_offset as u64)
            .await
            .unwrap();

        let mut chunk_set = JoinSet::new();

        for i in 0..total_chunks {
            let this_start = posting_list_offsets[start / FM_CHUNK_TOKS + i];
            let this_end = posting_list_offsets[start / FM_CHUNK_TOKS + i + 1];
            let this_chunk = plist_chunks
                [(this_start - start_offset) as usize..(this_end - start_offset) as usize]
                .to_vec();

            chunk_set.spawn(async move {
                let mut decompressor = Decoder::new(&this_chunk[..]).unwrap();
                let mut serialized_plist_chunk: Vec<u8> = Vec::with_capacity(this_chunk.len());
                decompressor
                    .read_to_end(&mut serialized_plist_chunk)
                    .unwrap();
                let plist_chunk: Vec<u64> = bincode::deserialize(&serialized_plist_chunk).unwrap();

                let chunk_res: Vec<(u64, u64)> = if i == 0 {
                    if total_chunks == 1 {
                        plist_chunk[start % FM_CHUNK_TOKS..end % FM_CHUNK_TOKS]
                            .iter()
                            .map(|&uid| (file_id, uid))
                            .collect()
                    } else {
                        plist_chunk[start % FM_CHUNK_TOKS..]
                            .iter()
                            .map(|&uid| (file_id, uid))
                            .collect()
                    }
                } else if i == total_chunks - 1 {
                    plist_chunk[..end % FM_CHUNK_TOKS]
                        .iter()
                        .map(|&uid| (file_id, uid))
                        .collect()
                } else {
                    plist_chunk.iter().map(|&uid| (file_id, uid)).collect()
                };

                chunk_res
            });
        }

        while let Some(chunk_res) = chunk_set.join_next().await {
            res.extend(chunk_res.unwrap());
        }
    }

    // let mut res = Vec::new();
    // while let Some(query_res) = query_set.join_next().await {
    //     res.extend(query_res.unwrap());
    // }

    Ok(res)
}

async fn search_substring_one_file<T>(
    file_id: u64,
    mut reader: AsyncReader,
    file_size: usize,
    queries: Vec<Vec<T>>,
) -> Result<Vec<(u64, u64)>, LavaError>
where
    T: PrimInt
        + Unsigned
        + Serialize
        + for<'de> Deserialize<'de>
        + Clone
        + Eq
        + std::hash::Hash
        + AsPrimitive<usize>
        + Debug
        + Send
        + 'static,
    usize: AsPrimitive<T>,
{
    println!("{:?}", queries);

    let results = reader.read_usize_from_end(4).await?;
    let fm_chunk_offsets_offset = results[0];
    let posting_list_offsets_offset = results[1];
    let total_counts_offset = results[2];
    let n = results[3];

    let fm_chunk_offsets: Vec<u64> = reader
        .read_range_and_decompress(fm_chunk_offsets_offset, posting_list_offsets_offset)
        .await?;
    let posting_list_offsets: Vec<u64> = reader
        .read_range_and_decompress(posting_list_offsets_offset, total_counts_offset)
        .await?;
    let cumulative_counts: Vec<u64> = reader
        .read_range_and_decompress(total_counts_offset, (file_size - 32) as u64)
        .await?;

    let mut query_set = JoinSet::new();

    for query in queries {
        let fm_chunk_offsets = fm_chunk_offsets.clone();
        let cumulative_counts = cumulative_counts.clone();
        let posting_list_offsets = posting_list_offsets.clone();
        let mut reader = reader.clone();

        query_set.spawn(async move {
            process_substring_query::<T>(
                query,
                n,
                &fm_chunk_offsets,
                &cumulative_counts,
                &posting_list_offsets,
                &mut reader,
                file_id,
            )
            .await
        });
    }

    let mut res = Vec::new();
    while let Some(query_res) = query_set.join_next().await {
        res.extend(query_res.unwrap());
    }
    Ok(res)
}

pub async fn _search_lava_substring_char(
    files: Vec<String>,
    query: String,
    k: usize,
    reader_type: ReaderType,
    token_viable_limit: Option<usize>,
    sample_factor: Option<usize>,
    wavelet_tree: bool,
) -> Result<Vec<(u64, u64)>, LavaError> {
    let lower: String = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let result: Vec<u8> = lower
        .chars()
        .filter(|id| !SKIP.chars().contains(id))
        .map(|c| c as u8)
        .collect();

    let mut query: Vec<Vec<u8>> = if let Some(sample_factor) = sample_factor {
        (0..sample_factor)
            .map(|offset| {
                result
                    .iter()
                    .skip(offset)
                    .step_by(sample_factor)
                    .cloned()
                    .collect::<Vec<u8>>()
            })
            .filter(|vec| !vec.is_empty())
            .collect()
    } else {
        vec![result]
    };

    // println!("query {:?}", query);

    // query = [i[-token_viable_limit:] for i in query]
    if let Some(token_viable_limit) = token_viable_limit {
        query.iter_mut().for_each(|vec| {
            if vec.len() > token_viable_limit {
                *vec = vec
                    .iter()
                    .rev()
                    .take(token_viable_limit)
                    .rev()
                    .cloned()
                    .collect();
            }
        });
    }

    // println!("query {:?}", query);

    let (file_sizes, readers) = get_file_sizes_and_readers(&files, reader_type).await?;
    search_generic_async(
        file_sizes,
        readers,
        if wavelet_tree {
            QueryParam::SubstringCharWavelet(query)
        } else {
            QueryParam::SubstringChar(query)
        },
        k,
    )
    .await
}