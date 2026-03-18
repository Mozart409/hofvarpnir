-- Add password_hash column to users table for authentication
ALTER TABLE users ADD COLUMN password_hash TEXT NOT NULL DEFAULT '';
