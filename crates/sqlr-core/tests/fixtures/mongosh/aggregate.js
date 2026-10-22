db.sales.aggregate([
  { $match: { date: { $gte: new Date("2014-01-01"), $lt: new Date("2015-01-01") } } },
  { $group: { _id: "$item", total: { $sum: { $multiply: ["$price", "$quantity"] } } } },
  { $sort: { total: -1 } },
], { allowDiskUse: true })
