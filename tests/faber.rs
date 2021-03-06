use aries_askar::future::block_on;
use aries_askar::indy_compat::print_records;

#[test]
fn faber_print_records() {
    let db = "tests/faber.agent372766/sqlite.db";
    let key = "Faber.Agent372766";
    block_on(print_records(db, key)).unwrap();
}
