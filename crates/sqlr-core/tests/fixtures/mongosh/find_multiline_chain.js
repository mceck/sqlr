// From the mongosh docs: a cursor chained over several lines.
db.inventory
  .find(
    { tags: { $all: ['red', 'blank'] } }, // filter
    /* projection */ { item: 1 },
  )
  .projection({ item: 1, size: 1 })
  .sort({ item: 1 })
  .limit(5)
  .pretty();
