-- Exact candidate query at 23dee7bb, before filtering physical run boundaries.
SELECT data FROM run_views AS runs WHERE status='running'
    OR json_extract(data,'$.backup.requestedRevision') > COALESCE(json_extract(data,'$.backup.acknowledgedRevision'),0)
    OR (status IN ('succeeded','failed','cancelled','interrupted','queued','waiting')
         AND json_extract(data,'$.storage.mode')='on-demand'
         AND (json_extract(data,'$.storage.dirtyBytes')>0
              OR json_extract(data,'$.backup.status') IN ('pending','saving','error')))
    OR (json_extract(data,'$.storage.mode')='on-demand'
        AND json_extract(data,'$.backup.id') IS NOT NULL
        AND COALESCE(json_extract(data,'$.backup.blockEnvelope'),2)<>3)
