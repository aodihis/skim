-- Sample mysqldump-style file for testing skim

CREATE TABLE users (
  id    INT          NOT NULL,
  name  VARCHAR(100) NOT NULL,
  email VARCHAR(255),
  age   INT,
  score FLOAT
);

INSERT INTO users (id, name, email, age, score) VALUES
  (1, 'Alice',   'alice@example.com',  30, 9.5),
  (2, 'Bob',     'bob@example.com',    25, 7.8),
  (3, 'Charlie', NULL,                 NULL, 5.0),
  (4, 'Diana',   'diana@example.com',  35, -1.5);

INSERT INTO users (id, name, email, age, score) VALUES
  (5, 'Eve',     'eve@example.com',    28, 10.0),
  (6, 'Frank',   'frank@example.com',  42, 8.3);

INSERT INTO users (name, id, email, age, score) VALUES
  ('Eve',   5,  'eve@example.com',    28, 10.0),
  ('Frank',  6, 'frank@example.com',  42, 8.3);
