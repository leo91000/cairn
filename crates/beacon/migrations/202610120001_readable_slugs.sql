CREATE FUNCTION cairn_readable_slug() RETURNS text
LANGUAGE sql VOLATILE AS $$
    SELECT (ARRAY['bright', 'calm', 'clear', 'gentle', 'happy', 'little', 'lucky', 'merry', 'quiet', 'silver', 'soft', 'sunny', 'swift', 'warm', 'wild', 'wise'])[1 + floor(random() * 16)::int] || '-' ||
           (ARRAY['birch', 'brook', 'cedar', 'comet', 'coral', 'dawn', 'fern', 'finch', 'fox', 'maple', 'moon', 'otter', 'pine', 'robin', 'star', 'willow'])[1 + floor(random() * 16)::int] || '-' ||
           substr(replace(gen_random_uuid()::text, '-', ''), 25, 8)
$$;

ALTER TABLE installations ADD COLUMN slug text;
CREATE UNIQUE INDEX installations_slug ON installations(slug);

DO $$
DECLARE installation_id text;
BEGIN
    FOR installation_id IN SELECT id FROM installations WHERE slug IS NULL LOOP
        LOOP
            BEGIN
                UPDATE installations SET slug = cairn_readable_slug() WHERE id = installation_id;
                EXIT;
            EXCEPTION WHEN unique_violation THEN
                -- Retry a random collision without changing the installation identity.
            END;
        END LOOP;
    END LOOP;
END $$;

ALTER TABLE installations ALTER COLUMN slug SET DEFAULT cairn_readable_slug();
ALTER TABLE installations ALTER COLUMN slug SET NOT NULL;
