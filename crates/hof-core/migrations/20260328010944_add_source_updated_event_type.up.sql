-- Add source_updated variant to activity_event_type enum
ALTER TYPE activity_event_type ADD VALUE IF NOT EXISTS 'source_updated' AFTER 'source_created';
