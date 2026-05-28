"""
Generate a realistic mysqldump-style SQL fixture for integration testing.

Tables:
  users      ~100 000 rows  (id, username, email, age, balance, active, bio, created_at)
  products   ~  50 000 rows  (id, sku, name, category, price, stock, description, created_at)
  orders     ~ 150 000 rows  (id, user_id, product_id, quantity, unit_price, status, notes, ordered_at)

Targets ~35-45 MB uncompressed; stays well under GitHub's 100 MB file limit.
Matches the INSERT INTO ... VALUES (...),(...) format that mysqldump produces.
"""

import random
import sys
import os
from datetime import datetime, timedelta

BATCH = 500   # rows per INSERT statement (mysqldump default is ~500-1000)

CATEGORIES = ["Electronics", "Clothing", "Books", "Food", "Sports", "Home", "Toys", "Beauty"]
STATUSES   = ["pending", "processing", "shipped", "delivered", "cancelled", "refunded"]
DOMAINS    = ["gmail.com", "yahoo.com", "hotmail.com", "example.com", "test.org", "mail.net"]

FIRST = ["Alice","Bob","Carol","David","Eve","Frank","Grace","Hank","Iris","Jack",
         "Karen","Liam","Mia","Noah","Olivia","Paul","Quinn","Rita","Sam","Tara",
         "Uma","Victor","Wendy","Xander","Yara","Zach"]
LAST  = ["Smith","Jones","Brown","Taylor","Wilson","Moore","Anderson","Thomas",
         "Jackson","White","Harris","Martin","Lee","Walker","Hall","Allen","Young"]

WORDS = ["the","quick","brown","fox","jumps","over","lazy","dog","lorem","ipsum",
         "dolor","sit","amet","consectetur","adipiscing","elit","sed","do","eiusmod",
         "tempor","incididunt","labore","dolore","magna","aliqua"]

rng = random.Random(42)   # fixed seed → reproducible file

def esc(s):
    return s.replace("\\", "\\\\").replace("'", "\\'")

def rand_date(start_year=2020, end_year=2024):
    base = datetime(start_year, 1, 1)
    delta = timedelta(days=rng.randint(0, 365 * (end_year - start_year + 1)))
    return (base + delta).strftime("%Y-%m-%d %H:%M:%S")

def rand_bio():
    n = rng.randint(5, 15)
    return " ".join(rng.choices(WORDS, k=n))

def rand_sku(i):
    return f"SKU-{rng.choice(['A','B','C','D'])}{i:06d}"

def rand_product_name():
    adj = rng.choice(["Premium","Basic","Elite","Standard","Pro","Lite","Ultra","Mini"])
    noun = rng.choice(["Widget","Gadget","Device","Unit","Module","Component","Kit","Pack"])
    return f"{adj} {noun} {rng.randint(100,999)}"

def rand_notes():
    if rng.random() < 0.3:
        return "NULL"
    n = rng.randint(3, 10)
    txt = " ".join(rng.choices(WORDS, k=n))
    return f"'{esc(txt)}'"

def write_header(f):
    f.write("-- MySQL dump (generated fixture for skim integration tests)\n")
    f.write("-- Generated: {}\n".format(datetime.utcnow().isoformat()))
    f.write("--\n\n")
    f.write("SET NAMES utf8mb4;\n")
    f.write("SET FOREIGN_KEY_CHECKS=0;\n\n")

def write_users(f, n=100_000):
    f.write("-- Table: users\n")
    f.write("DROP TABLE IF EXISTS `users`;\n")
    f.write(
        "CREATE TABLE `users` (\n"
        "  `id`         INT NOT NULL AUTO_INCREMENT,\n"
        "  `username`   VARCHAR(50) NOT NULL,\n"
        "  `email`      VARCHAR(120) NOT NULL,\n"
        "  `age`        TINYINT UNSIGNED DEFAULT NULL,\n"
        "  `balance`    DECIMAL(12,2) NOT NULL DEFAULT '0.00',\n"
        "  `active`     TINYINT(1) NOT NULL DEFAULT 1,\n"
        "  `bio`        TEXT DEFAULT NULL,\n"
        "  `created_at` DATETIME NOT NULL,\n"
        "  PRIMARY KEY (`id`)\n"
        ") ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\n"
    )

    rows = []
    for i in range(1, n + 1):
        first   = rng.choice(FIRST)
        last    = rng.choice(LAST)
        uname   = f"{first.lower()}{last.lower()}{i}"
        email   = f"{uname}@{rng.choice(DOMAINS)}"
        age     = "NULL" if rng.random() < 0.05 else str(rng.randint(18, 80))
        balance = f"{rng.uniform(0, 50000):.2f}"
        active  = "1" if rng.random() < 0.85 else "0"
        bio     = "NULL" if rng.random() < 0.2 else f"'{esc(rand_bio())}'"
        created = rand_date()
        rows.append(f"({i},'{esc(uname)}','{esc(email)}',{age},{balance},{active},{bio},'{created}')")

        if len(rows) == BATCH or i == n:
            f.write(f"INSERT INTO `users` VALUES\n")
            f.write(",\n".join(rows))
            f.write(";\n")
            rows = []
            if i % 10_000 == 0:
                print(f"  users: {i}/{n}", file=sys.stderr)

    f.write("\n")

def write_products(f, n=50_000):
    f.write("-- Table: products\n")
    f.write("DROP TABLE IF EXISTS `products`;\n")
    f.write(
        "CREATE TABLE `products` (\n"
        "  `id`          INT NOT NULL AUTO_INCREMENT,\n"
        "  `sku`         VARCHAR(20) NOT NULL,\n"
        "  `name`        VARCHAR(200) NOT NULL,\n"
        "  `category`    VARCHAR(50) NOT NULL,\n"
        "  `price`       DECIMAL(10,2) NOT NULL,\n"
        "  `stock`       INT NOT NULL DEFAULT 0,\n"
        "  `description` TEXT DEFAULT NULL,\n"
        "  `created_at`  DATETIME NOT NULL,\n"
        "  PRIMARY KEY (`id`)\n"
        ") ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\n"
    )

    rows = []
    for i in range(1, n + 1):
        sku   = rand_sku(i)
        name  = rand_product_name()
        cat   = rng.choice(CATEGORIES)
        price = f"{rng.uniform(0.99, 999.99):.2f}"
        stock = rng.randint(0, 5000)
        desc  = "NULL" if rng.random() < 0.15 else f"'{esc(rand_bio())}'"
        created = rand_date()
        rows.append(f"({i},'{sku}','{esc(name)}','{cat}',{price},{stock},{desc},'{created}')")

        if len(rows) == BATCH or i == n:
            f.write(f"INSERT INTO `products` VALUES\n")
            f.write(",\n".join(rows))
            f.write(";\n")
            rows = []

    f.write("\n")

def write_orders(f, n=150_000, max_user=100_000, max_product=50_000):
    f.write("-- Table: orders\n")
    f.write("DROP TABLE IF EXISTS `orders`;\n")
    f.write(
        "CREATE TABLE `orders` (\n"
        "  `id`          INT NOT NULL AUTO_INCREMENT,\n"
        "  `user_id`     INT NOT NULL,\n"
        "  `product_id`  INT NOT NULL,\n"
        "  `quantity`    SMALLINT NOT NULL DEFAULT 1,\n"
        "  `unit_price`  DECIMAL(10,2) NOT NULL,\n"
        "  `status`      VARCHAR(20) NOT NULL DEFAULT 'pending',\n"
        "  `notes`       TEXT DEFAULT NULL,\n"
        "  `ordered_at`  DATETIME NOT NULL,\n"
        "  PRIMARY KEY (`id`)\n"
        ") ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\n"
    )

    rows = []
    for i in range(1, n + 1):
        uid    = rng.randint(1, max_user)
        pid    = rng.randint(1, max_product)
        qty    = rng.randint(1, 20)
        price  = f"{rng.uniform(0.99, 999.99):.2f}"
        status = rng.choice(STATUSES)
        notes  = rand_notes()
        ordered = rand_date()
        rows.append(f"({i},{uid},{pid},{qty},{price},'{status}',{notes},'{ordered}')")

        if len(rows) == BATCH or i == n:
            f.write(f"INSERT INTO `orders` VALUES\n")
            f.write(",\n".join(rows))
            f.write(";\n")
            rows = []
            if i % 10_000 == 0:
                print(f"  orders: {i}/{n}", file=sys.stderr)

    f.write("\n")

def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "fixture.sql"
    print(f"Writing to {out} ...", file=sys.stderr)
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        write_header(f)
        write_users(f)
        write_products(f)
        write_orders(f)
        f.write("SET FOREIGN_KEY_CHECKS=1;\n")
    size_mb = os.path.getsize(out) / 1024 / 1024
    print(f"Done. {size_mb:.1f} MB", file=sys.stderr)

if __name__ == "__main__":
    main()
