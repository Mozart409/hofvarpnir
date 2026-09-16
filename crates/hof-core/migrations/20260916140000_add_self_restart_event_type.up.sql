-- Add self_restart variant to activity_event_type enum
ALTER TYPE activity_event_type ADD VALUE IF NOT EXISTS 'self_restart' AFTER 'source_deleted';
