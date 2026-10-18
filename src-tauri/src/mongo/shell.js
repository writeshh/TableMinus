// The mongosh-like world a script runs in (mongo/shell.rs). Nothing here reaches the server: `db.<collection>.<method>`
// calls are recorded as operations, which the app then runs in order. Values go back as Extended JSON, so
// ObjectId("…"), ISODate("…"), NumberLong(…), regular expressions and the rest keep their BSON types.
"use strict";
const __ops = [];
const __record = (o) => (__ops.length >= 100 ? (() => { throw new Error("a script may run at most 100 operations"); })() : __ops.push(o), o);

function ObjectId(hex) {
  if (!(this instanceof ObjectId)) return new ObjectId(hex);
  if (hex === undefined) {
    hex = Math.floor(Date.now() / 1000).toString(16).padStart(8, "0");
    for (let i = 0; i < 16; i++) hex += "0123456789abcdef"[Math.floor(Math.random() * 16)];
  }
  if (typeof hex !== "string" || !/^[0-9a-fA-F]{24}$/.test(hex)) throw new Error("ObjectId takes 24 hexadecimal digits");
  this.$oid = hex.toLowerCase();
}
ObjectId.prototype.toString = function () { return `ObjectId("${this.$oid}")`; };
ObjectId.prototype.toHexString = function () { return this.$oid; };
function ISODate(s) {
  const d = s === undefined ? new Date() : new Date(s);
  if (isNaN(d.getTime())) throw new Error(`ISODate: not a date: ${s}`);
  return d;
}
const NumberLong = (n) => ({ $numberLong: String(BigInt(n)) });
const NumberInt = (n) => ({ $numberInt: String(Number(n) | 0) });
const NumberDecimal = (n) => ({ $numberDecimal: String(n) });
const Decimal128 = NumberDecimal;
const Long = NumberLong;
const Int32 = NumberInt;
const Double = (n) => Number(n);
const UUID = (s) => ({ $uuid: s === undefined ? crypto_uuid() : String(s) });
const BinData = (subtype, base64) => ({ $binary: { base64: String(base64), subType: Number(subtype).toString(16).padStart(2, "0") } });
const Timestamp = (t, i) => (typeof t === "object" ? { $timestamp: { t: t.t, i: t.i } } : { $timestamp: { t: t ?? 0, i: i ?? 0 } });
const MinKey = () => ({ $minKey: 1 });
const MaxKey = () => ({ $maxKey: 1 });
function crypto_uuid() {
  const h = [...Array(32)].map(() => "0123456789abcdef"[Math.floor(Math.random() * 16)]).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-4${h.slice(13, 16)}-a${h.slice(17, 20)}-${h.slice(20)}`;
}

/** An operation, with the chainable methods of mongosh's cursors and results. */
function Op(fields) { Object.assign(this, fields); }
Op.prototype.sort = function (s) { this.sort_ = s; return this; };
Op.prototype.skip = function (n) { this.skip_ = n; return this; };
Op.prototype.limit = function (n) { this.limit_ = n; return this; };
Op.prototype.projection = Op.prototype.project = function (p) { this.projection_ = p; return this; };
Op.prototype.count = function () { this.op = "countDocuments"; return this; };
Op.prototype.size = Op.prototype.itcount = Op.prototype.count;
Op.prototype.explain = function () { this.explain_ = true; return this; };
for (const ignored of ["toArray", "pretty", "batchSize", "hint", "maxTimeMS", "readPref", "allowDiskUse", "comment", "noCursorTimeout"]) {
  Op.prototype[ignored] = function () { return this; };
}
Op.prototype.toJSON = function () {
  const out = {};
  for (const [k, v] of Object.entries(this)) if (v !== undefined) out[k.endsWith("_") ? k.slice(0, -1) : k] = v;
  return out;
};

function Collection(db, name) { this.__db = db; this.__name = name; }
const c = Collection.prototype;
const op = (coll, name, fields) => __record(new Op({ op: name, db: coll.__db, collection: coll.__name, ...fields }));
// Fields a chained method also sets end in _ (toJSON drops it), so they don't hide the method.
c.find = function (filter, projection) { return op(this, "find", { filter, projection_: projection }); };
c.findOne = function (filter, projection) { return op(this, "find", { filter, projection_: projection, limit_: 1 }); };
c.aggregate = function (pipeline, options) { return op(this, "aggregate", { pipeline: Array.isArray(pipeline) ? pipeline : [...arguments], options }); };
c.countDocuments = function (filter) { return op(this, "countDocuments", { filter }); };
c.count = c.countDocuments;
c.estimatedDocumentCount = function () { return op(this, "estimatedDocumentCount", {}); };
c.distinct = function (field, filter) { return op(this, "distinct", { field, filter }); };
c.insertOne = function (document) { return op(this, "insertOne", { document }); };
c.insertMany = function (documents) { return op(this, "insertMany", { documents }); };
c.insert = function (d) { return Array.isArray(d) ? this.insertMany(d) : this.insertOne(d); };
c.updateOne = function (filter, update, options) { return op(this, "updateOne", { filter, update, options }); };
c.updateMany = function (filter, update, options) { return op(this, "updateMany", { filter, update, options }); };
c.replaceOne = function (filter, replacement, options) { return op(this, "replaceOne", { filter, replacement, options }); };
c.deleteOne = function (filter) { return op(this, "deleteOne", { filter }); };
c.deleteMany = function (filter) { return op(this, "deleteMany", { filter }); };
c.remove = function (filter, justOne) { return justOne ? this.deleteOne(filter) : this.deleteMany(filter); };
c.findOneAndUpdate = function (filter, update, options) { return op(this, "findOneAndUpdate", { filter, update, options }); };
c.findOneAndReplace = function (filter, replacement, options) { return op(this, "findOneAndReplace", { filter, replacement, options }); };
c.findOneAndDelete = function (filter, options) { return op(this, "findOneAndDelete", { filter, options }); };
c.createIndex = function (keys, options) { return op(this, "createIndex", { keys, options }); };
c.ensureIndex = c.createIndex;
c.dropIndex = function (index) { return op(this, "dropIndex", { index }); };
c.getIndexes = function () { return op(this, "getIndexes", {}); };
c.drop = function () { return op(this, "drop", {}); };
c.renameCollection = function (to) { return op(this, "renameCollection", { to }); };
c.stats = function () { return op(this, "runCommand", { command: { collStats: this.__name } }); };
c.getName = function () { return this.__name; };

function Db(name) { this.__name = name; }
const d = Db.prototype;
const dbOp = (db, name, fields) => __record(new Op({ op: name, db: db.__name, ...fields }));
d.getCollection = function (name) { return new Collection(this.__name, String(name)); };
d.getSiblingDB = function (name) { return dbNamed(String(name)); };
d.getName = function () { return this.__name; };
d.getCollectionNames = function () { return dbOp(this, "listCollections", {}); };
d.getCollectionInfos = d.getCollectionNames;
d.createCollection = function (name, options) { return dbOp(this, "createCollection", { name: String(name), options }); };
d.dropDatabase = function () { return dbOp(this, "dropDatabase", {}); };
d.runCommand = function (command) { return dbOp(this, "runCommand", { command: typeof command === "string" ? { [command]: 1 } : command }); };
d.adminCommand = function (command) { return __record(new Op({ op: "runCommand", db: "admin", command: typeof command === "string" ? { [command]: 1 } : command })); };
d.stats = function () { return this.runCommand({ dbStats: 1 }); };
d.serverStatus = function () { return this.adminCommand({ serverStatus: 1 }); };
d.version = function () { return this.adminCommand({ buildInfo: 1 }); };
d.getUsers = function () { return this.runCommand({ usersInfo: 1 }); };

/** `db`, with any other property a collection of that name (`db.orders`). */
const dbNamed = (name) => new Proxy(new Db(name), {
  get(target, key) {
    if (typeof key !== "string" || key in target || key === "then" || key === "toJSON") return target[key];
    return new Collection(target.__name, key);
  },
});

globalThis.db = dbNamed(__database);
globalThis.__show = (what) => {
  if (what === "dbs" || what === "databases") __record(new Op({ op: "listDatabases", db: db.getName() }));
  else if (what === "collections" || what === "tables") db.getCollectionNames();
  else if (what === "users") db.getUsers();
  else if (what === "roles") db.runCommand({ rolesInfo: 1 });
  else throw new Error(`show ${what}: try show dbs, show collections, show users or show roles`);
};
globalThis.__use = (name) => {
  __record(new Op({ op: "use", db: name }));
  globalThis.db = dbNamed(name);
};

/** Extended JSON for what JSON can't say: dates, regular expressions, big integers. */
function __ext(key, value) {
  const raw = this[key];
  if (raw instanceof Date) return { $date: raw.toISOString() };
  if (raw instanceof RegExp) return { $regularExpression: { pattern: raw.source, options: raw.flags.replace(/[^imsux]/g, "") } };
  if (typeof raw === "bigint") return { $numberLong: raw.toString() };
  if (typeof raw === "function") throw new Error("a function can't be sent to the server");
  return value;
}
