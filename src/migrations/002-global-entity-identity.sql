  ALTER TABLE events ADD COLUMN source TEXT;
  UPDATE events
  SET source = (SELECT source FROM entities WHERE entities.id = events.entity_id);
  
  CREATE TEMP TABLE entity_merge AS
  SELECT entity.id AS old_id,
    (SELECT MIN(candidate.id)
     FROM entities candidate
     WHERE candidate.external_id = entity.external_id) AS canonical_id
  FROM entities entity;
  
  CREATE TEMP TABLE event_merge AS
  SELECT event.id AS old_id,
    (SELECT MIN(candidate.id)
     FROM events candidate
     JOIN entity_merge candidate_entity ON candidate_entity.old_id = candidate.entity_id
     WHERE candidate_entity.canonical_id = event_entity.canonical_id
       AND candidate.revision_id = event.revision_id) AS canonical_id
  FROM events event
  JOIN entity_merge event_entity ON event_entity.old_id = event.entity_id;
  
  UPDATE command_events
  SET event_id = (
    SELECT canonical_id FROM event_merge WHERE old_id = command_events.event_id
  )
  WHERE event_id IN (SELECT old_id FROM event_merge WHERE old_id != canonical_id);
  
  DELETE FROM events
  WHERE id IN (SELECT old_id FROM event_merge WHERE old_id != canonical_id);
  
  UPDATE events
  SET entity_id = (
    SELECT canonical_id FROM entity_merge WHERE old_id = events.entity_id
  );
  
  UPDATE entities AS canonical
  SET aven_ref = COALESCE(
        (SELECT duplicate.aven_ref
         FROM entities duplicate
         JOIN entity_merge mapping ON mapping.old_id = duplicate.id
         WHERE mapping.canonical_id = canonical.id
           AND duplicate.aven_ref IS NOT NULL
         ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
         LIMIT 1),
        canonical.aven_ref
      ),
      investigation_handle = COALESCE(
        (SELECT duplicate.investigation_handle
         FROM entities duplicate
         JOIN entity_merge mapping ON mapping.old_id = duplicate.id
         WHERE mapping.canonical_id = canonical.id
           AND duplicate.investigation_handle IS NOT NULL
         ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
         LIMIT 1),
        canonical.investigation_handle
      ),
      kind = (SELECT duplicate.kind
              FROM entities duplicate
              JOIN entity_merge mapping ON mapping.old_id = duplicate.id
              WHERE mapping.canonical_id = canonical.id
              ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
              LIMIT 1),
      title = (SELECT duplicate.title
               FROM entities duplicate
               JOIN entity_merge mapping ON mapping.old_id = duplicate.id
               WHERE mapping.canonical_id = canonical.id
               ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
               LIMIT 1),
      last_event_at = (SELECT MAX(duplicate.last_event_at)
                       FROM entities duplicate
                       JOIN entity_merge mapping ON mapping.old_id = duplicate.id
                       WHERE mapping.canonical_id = canonical.id),
      handling_status = (SELECT duplicate.handling_status
                         FROM entities duplicate
                         JOIN entity_merge mapping ON mapping.old_id = duplicate.id
                         WHERE mapping.canonical_id = canonical.id
                         ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
                         LIMIT 1),
      operational_metadata = (SELECT duplicate.operational_metadata
                              FROM entities duplicate
                              JOIN entity_merge mapping ON mapping.old_id = duplicate.id
                              WHERE mapping.canonical_id = canonical.id
                              ORDER BY duplicate.last_event_at DESC, duplicate.id DESC
                              LIMIT 1)
  WHERE canonical.id IN (SELECT canonical_id FROM entity_merge);
  
  DELETE FROM entities
  WHERE id IN (SELECT old_id FROM entity_merge WHERE old_id != canonical_id);
  
  CREATE UNIQUE INDEX entities_external_id_idx ON entities(external_id);
  DROP TABLE event_merge;
  DROP TABLE entity_merge;
