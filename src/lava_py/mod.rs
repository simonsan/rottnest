use pyo3::prelude::*;

mod format;
mod lava;
mod logcloud;

#[pymodule]
fn rottnest(_py: Python, m: &PyModule) -> PyResult<()> {
    pyo3_log::init();

    m.add_function(wrap_pyfunction!(lava::build_lava_bm25, m)?)?;
    m.add_function(wrap_pyfunction!(lava::build_lava_uuid, m)?)?;
    m.add_function(wrap_pyfunction!(lava::build_lava_substring, m)?)?;
    m.add_function(wrap_pyfunction!(lava::search_lava_bm25, m)?)?;
    m.add_function(wrap_pyfunction!(lava::search_lava_substring, m)?)?;
    m.add_function(wrap_pyfunction!(lava::search_lava_vector, m)?)?;
    m.add_function(wrap_pyfunction!(lava::search_lava_uuid, m)?)?;
    m.add_function(wrap_pyfunction!(lava::get_tokenizer_vocab, m)?)?;
    m.add_function(wrap_pyfunction!(lava::merge_lava_generic, m)?)?;
    m.add_function(wrap_pyfunction!(format::get_parquet_layout, m)?)?;
    m.add_function(wrap_pyfunction!(format::read_indexed_pages, m)?)?;
    m.add_function(wrap_pyfunction!(format::populate_cache, m)?)?;

    m.add_function(wrap_pyfunction!(logcloud::index_logcloud, m)?)?;
    m.add_function(wrap_pyfunction!(logcloud::search_logcloud, m)?)?;
    m.add_function(wrap_pyfunction!(logcloud::index_analysis, m)?)?;
    m.add_function(wrap_pyfunction!(logcloud::compress_logs, m)?)?;

    Ok(())
}
