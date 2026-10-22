db.people.createIndex({ last: 1, first: -1 }, { unique: true, name: "by_name" });
db.people.dropIndex("by_name");
db.people.dropIndex({ age: 1 });
db.people.getIndexes();
db.people.drop();
