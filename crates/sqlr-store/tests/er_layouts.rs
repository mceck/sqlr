use sqlr_store::Db;

#[test]
fn er_layout_round_trip() {
    let db = Db::open_in_memory().unwrap();
    assert_eq!(db.load_er_layout("pg-1|app|public|abc").unwrap(), None);
    db.save_er_layout("pg-1|app|public|abc", "[[0,0],[10,20]]")
        .unwrap();
    db.save_er_layout("pg-1|app|other|abc", "[[1,1]]").unwrap();
    assert_eq!(
        db.load_er_layout("pg-1|app|public|abc").unwrap().as_deref(),
        Some("[[0,0],[10,20]]")
    );
    // Saving again replaces; keys are independent.
    db.save_er_layout("pg-1|app|public|abc", "[[5,5]]").unwrap();
    assert_eq!(
        db.load_er_layout("pg-1|app|public|abc").unwrap().as_deref(),
        Some("[[5,5]]")
    );
    assert_eq!(
        db.load_er_layout("pg-1|app|other|abc").unwrap().as_deref(),
        Some("[[1,1]]")
    );
}
