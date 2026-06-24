diesel::table! {
    items (id) {
        id -> Uuid,
        name -> Text,
        created_at -> Timestamp,
    }
}
