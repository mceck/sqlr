db.runCommand({ listCollections: 1, filter: {}, nameOnly: true })
db.getCollectionNames()
db.runCommand(
  {
    find: "c",
    filter: { "a.b": { $exists: true } },
  }
)
