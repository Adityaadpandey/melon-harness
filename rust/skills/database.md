---
name: database
description: Schema, migrations, indexes, queries and transactions
triggers: migration, schema, index, sql, query, queries, transaction, orm, prisma, database, column, table, foreign key
---
# Database changes
Schema and migration ship together; applied migrations are immutable (fix forward). Every new filter or join needs an index; on busy tables create it concurrently. Keep transactions short with no network calls inside. Bound every read. Make writes idempotent where retries exist. Never run destructive statements yourself: hand the user the SQL and say where to run it, whether it reads or writes, and what to paste back.
