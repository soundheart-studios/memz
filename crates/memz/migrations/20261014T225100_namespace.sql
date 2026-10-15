CREATE TABLE namespace_meta (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), mutation_seq INTEGER NOT NULL DEFAULT 0,
 quota_bytes INTEGER NOT NULL DEFAULT 67108864, quota_records INTEGER NOT NULL DEFAULT 100000
);
INSERT INTO namespace_meta(singleton) VALUES(1);
CREATE TABLE sources (
 id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0), origin TEXT NOT NULL,
 author TEXT NOT NULL, external_ref TEXT, text TEXT NOT NULL, content_hash TEXT NOT NULL,
 event_at INTEGER, ingested_at INTEGER NOT NULL, visibility TEXT NOT NULL CHECK(visibility IN ('project','session')),
 session_id TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('active','superseded','deleted')),
 retention TEXT NOT NULL CHECK(retention IN ('durable','temporary')), expires_at INTEGER,
 PRIMARY KEY(id,revision), CHECK((visibility='project' AND session_id='') OR (visibility='session' AND session_id<>'')),
 CHECK((retention='durable' AND expires_at IS NULL) OR (retention='temporary' AND expires_at IS NOT NULL))
);
CREATE TABLE items (
 id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0), kind TEXT NOT NULL,
 content TEXT NOT NULL, explicit INTEGER NOT NULL CHECK(explicit IN (0,1)),
 status TEXT NOT NULL CHECK(status IN ('active','superseded','invalid','deleted')),
 source_id TEXT NOT NULL, source_revision INTEGER NOT NULL, visibility TEXT NOT NULL,
 session_id TEXT NOT NULL, valid_from INTEGER NOT NULL, valid_to INTEGER, slot TEXT,
 PRIMARY KEY(id,revision), FOREIGN KEY(source_id,source_revision) REFERENCES sources(id,revision),
 CHECK(visibility IN ('project','session')),
 CHECK((visibility='project' AND session_id='') OR (visibility='session' AND session_id<>''))
);
CREATE UNIQUE INDEX items_one_active ON items(id) WHERE status='active';
CREATE INDEX items_scope_status ON items(visibility,session_id,status,valid_from);
CREATE INDEX items_source ON items(source_id,source_revision,status);
CREATE TABLE edges (
 id TEXT NOT NULL, revision INTEGER NOT NULL, from_id TEXT NOT NULL, from_revision INTEGER NOT NULL,
 to_id TEXT NOT NULL, to_revision INTEGER NOT NULL, relation TEXT NOT NULL,
 qualifiers TEXT NOT NULL DEFAULT '{}', explicit INTEGER NOT NULL CHECK(explicit IN (0,1)),
 status TEXT NOT NULL CHECK(status IN ('active','superseded','invalid','deleted')),
 visibility TEXT NOT NULL CHECK(visibility IN ('project','session')), session_id TEXT NOT NULL,
 valid_from INTEGER NOT NULL, valid_to INTEGER, PRIMARY KEY(id,revision),
 FOREIGN KEY(from_id,from_revision) REFERENCES items(id,revision),
 FOREIGN KEY(to_id,to_revision) REFERENCES items(id,revision)
);
CREATE INDEX edges_from ON edges(from_id,relation,status,visibility,session_id);
CREATE INDEX edges_to ON edges(to_id,relation,status,visibility,session_id);
CREATE TABLE evidence (
 id INTEGER PRIMARY KEY, item_id TEXT, item_revision INTEGER, edge_id TEXT, edge_revision INTEGER,
 source_id TEXT NOT NULL, source_revision INTEGER NOT NULL, span_start INTEGER, span_end INTEGER,
 role TEXT NOT NULL, method TEXT NOT NULL, method_version TEXT NOT NULL,
 FOREIGN KEY(item_id,item_revision) REFERENCES items(id,revision),
 FOREIGN KEY(edge_id,edge_revision) REFERENCES edges(id,revision),
 FOREIGN KEY(source_id,source_revision) REFERENCES sources(id,revision),
 CHECK((item_id IS NOT NULL AND item_revision IS NOT NULL AND edge_id IS NULL AND edge_revision IS NULL) OR
       (edge_id IS NOT NULL AND edge_revision IS NOT NULL AND item_id IS NULL AND item_revision IS NULL)),
 CHECK((span_start IS NULL AND span_end IS NULL) OR (span_start>=0 AND span_end>=span_start))
);
CREATE INDEX evidence_source ON evidence(source_id,source_revision);
CREATE INDEX evidence_item ON evidence(item_id,item_revision);
CREATE INDEX evidence_edge ON evidence(edge_id,edge_revision);
CREATE TABLE jobs (
 id TEXT PRIMARY KEY, operation_key TEXT NOT NULL UNIQUE, source_id TEXT NOT NULL, source_revision INTEGER NOT NULL,
 job_type TEXT NOT NULL, version INTEGER NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','leased','completed','failed','obsolete')),
 attempts INTEGER NOT NULL DEFAULT 0, lease_owner TEXT, lease_expiry INTEGER, next_attempt INTEGER NOT NULL,
 error_code TEXT, resource_bytes INTEGER NOT NULL DEFAULT 0, principal TEXT NOT NULL, capability_revision INTEGER NOT NULL,
 FOREIGN KEY(source_id,source_revision) REFERENCES sources(id,revision)
);
CREATE INDEX jobs_ready ON jobs(state,next_attempt,lease_expiry);
CREATE TABLE operations (
 principal TEXT NOT NULL, session_id TEXT NOT NULL, request_id TEXT NOT NULL, payload_digest TEXT NOT NULL,
 result_json TEXT NOT NULL, item_id TEXT NOT NULL, tombstone INTEGER NOT NULL DEFAULT 0 CHECK(tombstone IN (0,1)),
 mutation_seq INTEGER NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY(principal,session_id,request_id)
);
CREATE INDEX operations_item ON operations(item_id);
CREATE VIRTUAL TABLE memory_fts USING fts5(item_id UNINDEXED,revision UNINDEXED,content,tokenize='unicode61');
CREATE TABLE migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
INSERT INTO migrations(version,name) VALUES(1,'20261014T225100_namespace');
