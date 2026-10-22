db.orders.find({ status: "A", qty: { $lt: 30 } }, { item: 1, _id: 0 }).sort({ created: -1 }).skip(20).limit(10)
