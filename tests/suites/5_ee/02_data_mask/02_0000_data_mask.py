#!/usr/bin/env python3

import os
import mysql.connector
import sys

CURDIR = os.path.dirname(os.path.realpath(__file__))
sys.path.insert(0, os.path.join(CURDIR, "../../../helpers"))

from native_client import NativeClient  # NOQA
from native_client import prompt  # NOQA

log = None

# client1 send long query, client mydb fetch the long query and kill it.
# Use to mock in MySQL Client press Ctrl C to intercept a long query.

mydb = mysql.connector.connect(
    host="127.0.0.1", user="root", passwd="root", port="3307"
)


def get_license():
    return os.getenv("DATABEND_ENTERPRISE_LICENSE")


if __name__ == "__main__":
    with NativeClient(name="client1>") as client1:
        client1.expect(prompt)

        client1.send("set global enterprise_license='{}';".format(get_license()))
        client1.expect(prompt)

        client1.send("drop MASKING POLICY if exists mask")

        mycursor = mydb.cursor()
        mycursor.execute(
            "CREATE MASKING POLICY mask AS (val STRING,num int) RETURN STRING -> CASE WHEN "
            "current_role() IN ('ANALYST') THEN VAL ELSE '*********'END comment = 'this is a masking policy';"
        )

        mycursor = mydb.cursor()
        mycursor.execute("desc masking policy mask;")
        mask = mycursor.fetchall()[0]
        print(mask[0], mask[2], mask[3], mask[4], mask[5])

        mycursor.execute("drop MASKING POLICY if exists mask")
        mycursor.execute("desc masking policy mask;")
        mask = mycursor.fetchall()
        print(mask)

        client1.send("drop table if exists data_mask_test;")
        client1.expect(prompt)

        client1.send("create table data_mask_test(a int, b string);")
        client1.expect(prompt)

        sql = "insert into table data_mask_test(a,b) values(1, 'abc')"
        mycursor.execute(sql)
        mycursor.execute("select * from data_mask_test")
        data = mycursor.fetchall()
        print(data)

        mycursor = mydb.cursor()
        mycursor.execute(
            "CREATE MASKING POLICY maska AS (val int) RETURN int -> CASE WHEN "
            "current_role() IN ('ANALYST') THEN VAL ELSE 200 END comment = 'this is a masking policy';"
        )
        mycursor = mydb.cursor()
        mycursor.execute(
            "CREATE MASKING POLICY maskb AS (val STRING) RETURN STRING -> CASE WHEN "
            "current_role() IN ('ANALYST') THEN VAL ELSE '*********'END comment = 'this is a masking policy';"
        )

        sql = " alter table data_mask_test modify column b set masking policy maskb"
        mycursor.execute(sql)
        mycursor.execute("select * from data_mask_test")
        data = mycursor.fetchall()
        print(data)

        sql = " alter table data_mask_test modify column a set masking policy maska"
        mycursor.execute(sql)
        mycursor.execute("select * from data_mask_test")
        data = mycursor.fetchall()
        print(data)
