/* find users by name */
db.users.find({
  name: /^jo[/a-z]+\/x$/m, // a slash inside a class and an escaped one
  email: { $regex: "@example\\.com$", $options: "i" },
  'odd key': "a // not a comment",
});
