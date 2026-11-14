"""Made-up data for the video's "production": a billing schema with
people in it (names, emails, phones, birthdays, addresses, support
notes) for masking to replace, and Flyway's history table. Every name is
a random pairing from the lists below; nothing here is real.

    python3 video/seed/seed.py > seed.sql
"""
import random

random.seed(7)
FIRST = "Amara Ben Chloe Dev Elif Finn Grace Hugo Isla Jonah Kira Leo Maya Nico Olive Priya Quinn Rosa Sami Tess Umar Vera Wes Yara Zane".split()
LAST = "Abbott Brennan Castell Dunmore Ellery Fairlie Garside Hollin Ingram Jessop Kendrew Lowther Marsden Northam Oakley Pendle Quarrie Rushby Sefton Tolland Upwood Varley Whitlow Yardley".split()
STREETS = ["Mill Lane", "Church Road", "Station Street", "Victoria Avenue", "Park Road", "High Street", "Orchard Close", "Kings Row"]
TOWNS = [("Leeds", "LS6"), ("Bristol", "BS3"), ("York", "YO1"), ("Bath", "BA2"), ("Derby", "DE1"), ("Hull", "HU5")]

print("""
CREATE TABLE flyway_schema_history (installed_rank int PRIMARY KEY, version varchar(50), description varchar(200) NOT NULL, type varchar(20) NOT NULL, script varchar(1000) NOT NULL, checksum int, installed_by varchar(100) NOT NULL, installed_on timestamp NOT NULL DEFAULT now(), execution_time int NOT NULL, success boolean NOT NULL);
CREATE TABLE customers (id bigint PRIMARY KEY, first_name text NOT NULL, last_name text NOT NULL, email text NOT NULL UNIQUE, phone text, date_of_birth date, address_line1 text, city text, postcode text, created_at timestamptz NOT NULL);
CREATE TABLE invoices (id bigint PRIMARY KEY, customer_id bigint NOT NULL REFERENCES customers(id), number text NOT NULL UNIQUE, total numeric(12,2) NOT NULL, status text NOT NULL, issued_at timestamptz NOT NULL);
CREATE TABLE invoice_lines (id bigint PRIMARY KEY, invoice_id bigint NOT NULL REFERENCES invoices(id), description text NOT NULL, amount numeric(12,2) NOT NULL);
CREATE TABLE support_notes (id bigint PRIMARY KEY, customer_id bigint NOT NULL REFERENCES customers(id), body text NOT NULL, created_at timestamptz NOT NULL);
""")
for i in range(1, 43):
    print(f"INSERT INTO flyway_schema_history VALUES ({i}, '1.{i}', 'migration {i}', 'SQL', 'V1_{i}__migration_{i}.sql', {1000 + i}, 'billing', now(), 12, true);")
customers = []
for cid in range(2101, 2341):
    f, l = random.choice(FIRST), random.choice(LAST)
    street, (town, area) = random.choice(STREETS), random.choice(TOWNS)
    email = f"{f.lower()}.{l.lower()}.{cid}@example.com"
    phone = f"+44 7700 9{random.randint(10000, 99999)}"
    dob = f"{random.randint(1955, 2002)}-{random.randint(1, 12):02d}-{random.randint(1, 28):02d}"
    postcode = f"{area} {random.randint(1, 9)}{random.choice('ABDEFGHJ')}{random.choice('LNPQRSTUW')}"
    print(f"INSERT INTO customers VALUES ({cid}, '{f}', '{l}', '{email}', '{phone}', '{dob}', "
          f"'{random.randint(1, 180)} {street}', '{town}', '{postcode}', now() - interval '{random.randint(10, 900)} days');")
    customers.append((cid, f))
invoice, line = 48200, 900000
for cid, _ in customers:
    for _ in range(random.randint(1, 4)):
        invoice += 1
        total, lines = 0, []
        for _ in range(random.randint(1, 3)):
            line += 1
            amount = round(random.uniform(20, 900), 2)
            total += amount
            item = random.choice(["Pro plan", "Seats", "Support", "Storage", "Onboarding"])
            lines.append(f"INSERT INTO invoice_lines VALUES ({line}, {invoice}, '{item}', {amount});")
        status = random.choice(["open", "paid", "paid", "paid", "void"])
        print(f"INSERT INTO invoices VALUES ({invoice}, {cid}, 'INV-{invoice:06d}', {total:.2f}, '{status}', "
              f"now() - interval '{random.randint(1, 400)} days');")
        print("\n".join(lines))
for note, (cid, f) in enumerate(random.sample(customers, 60), start=1):
    print(f"INSERT INTO support_notes VALUES ({note}, {cid}, '{f} called about a refund for their last invoice; "
          f"call back on their mobile.', now() - interval '{random.randint(1, 200)} days');")
print("ANALYZE;")
