use crate::errors::*;

use diesel;
use diesel::expression::SqlLiteral;
use diesel::expression::sql_literal::sql;
use diesel::sql_types::Bool;
use diesel::prelude::*;
use crate::models::*;
use crate::schema::*;
use std::str::FromStr;
use crate::paths;
use crate::migrations;
use crate::worker;
use crate::workspaces::Workspace;

pub mod ttl;


#[derive(Debug)]
pub enum DbChange {
    Insert,
    Update(Update),
    None,
}

impl DbChange {
    pub fn is_some(&self) -> bool {
        match self {
            DbChange::None => false,
            _ => true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Family {
    Domain,
    Subdomain,
    IpAddr,
    SubdomainIpAddr,
    Url,
    Email,
    PhoneNumber,
    Device,
    Network,
    NetworkDevice,
    Account,
    Breach,
    BreachEmail,
}

impl FromStr for Family {
    type Err = Error;

    fn from_str(s: &str) -> Result<Family> {
        Ok(match s {
            "domain" => Family::Domain,
            "subdomain" => Family::Subdomain,
            "ipaddr" => Family::IpAddr,
            "subdomain-ipaddr" => Family::SubdomainIpAddr,
            "url" => Family::Url,
            "email" => Family::Email,
            "phonenumber" => Family::PhoneNumber,
            "device" => Family::Device,
            "network" => Family::Network,
            "network-device" => Family::NetworkDevice,
            "account" => Family::Account,
            "breach" => Family::Breach,
            "breach-email" => Family::BreachEmail,
            _ => bail!("Unknown object family"),
        })
    }
}

pub struct Database {
    name: Workspace,
    db: SqliteConnection,
}

impl Database {
    pub fn establish(name: Workspace) -> Result<Database> {
        let db = worker::spawn_fn("Connecting to database", || {
            Database::establish_quiet(name)
        }, false)?;

        Ok(db)
    }

    pub fn establish_quiet(name: Workspace) -> Result<Database> {
        let path = paths::data_dir()?.join(name.to_string() + ".db");
        let path = path.into_os_string().into_string()
            .map_err(|_| format_err!("Failed to convert db path to utf-8"))?;

        let db = SqliteConnection::establish(&path)
            .context("Failed to connect to database")?;
        migrations::run(&db)
            .context("Failed to run migrations")?;
        db.execute("PRAGMA journal_mode = WAL")
            .context("Failed to enable write ahead log")?;
        db.execute("PRAGMA foreign_keys = ON")
            .context("Failed to enforce foreign keys")?;

        Ok(Database {
            name,
            db,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn db(&self) -> &SqliteConnection {
        &self.db
    }

    /// Returns true if we didn't have this value yet
    pub fn insert_generic(&self, object: Insert) -> Result<Option<(DbChange, i32)>> {
        match object {
            Insert::Domain(object) => self.insert_struct(object),
            Insert::Subdomain(object) => self.insert_struct(object),
            Insert::IpAddr(object) => self.insert_struct(object),
            Insert::SubdomainIpAddr(object) => self.insert_subdomain_ipaddr_struct(&object),
            Insert::Url(object) => self.insert_struct(object),
            Insert::Email(object) => self.insert_struct(object),
            Insert::PhoneNumber(object) => self.insert_struct(object),
            Insert::Device(object) => self.insert_struct(object),
            Insert::Network(object) => self.insert_struct(object),
            Insert::NetworkDevice(object) => self.insert_network_device_struct(&object),
            Insert::Account(object) => self.insert_struct(object),
            Insert::Breach(object) => self.insert_struct(object),
            Insert::BreachEmail(object) => self.insert_breach_email_struct(object),
        }
    }

    /// Returns true if we didn't have this value yet
    pub fn insert_struct<T: InsertableStruct<M>, M: Model + Scopable>(&self, obj: T) -> Result<Option<(DbChange, i32)>> {
        if let Some(existing) = M::get_opt(self, obj.value())? {
            // entity is out of scope
            if !existing.scoped() {
                return Ok(None);
            }

            let update = obj.upsert(&existing);
            if update.is_dirty() {
                update.apply(&self)?;
                Ok(Some((DbChange::Update(update.generic()), existing.id())))
            } else {
                Ok(Some((DbChange::None, existing.id())))
            }
        } else {
            obj.insert(&self)?;
            let id = M::get_id(self, obj.value())?;
            Ok(Some((DbChange::Insert, id)))
        }
    }

    pub fn insert_subdomain_ipaddr_struct(&self, subdomain_ipaddr: &NewSubdomainIpAddr) -> Result<Option<(DbChange, i32)>> {
        if let Some(subdomain_ipaddr_id) = SubdomainIpAddr::get_id_opt(self, &(subdomain_ipaddr.subdomain_id, subdomain_ipaddr.ip_addr_id))? {
            Ok(Some((DbChange::None, subdomain_ipaddr_id)))
        } else {
            diesel::insert_into(subdomain_ipaddrs::table)
                .values(subdomain_ipaddr)
                .execute(&self.db)?;
            let id = SubdomainIpAddr::get_id(self, &(subdomain_ipaddr.subdomain_id, subdomain_ipaddr.ip_addr_id))?;
            Ok(Some((DbChange::Insert, id)))
        }
    }

    pub fn insert_network_device_struct(&self, network_device: &NewNetworkDevice) -> Result<Option<(DbChange, i32)>> {
        if let Some(network_device_id) = NetworkDevice::get_id_opt(self, &(network_device.network_id, network_device.device_id))? {
            Ok(Some((DbChange::None, network_device_id)))
        } else {
            diesel::insert_into(network_devices::table)
                .values(network_device)
                .execute(&self.db)?;
            let id = NetworkDevice::get_id(self, &(network_device.network_id, network_device.device_id))?;
            Ok(Some((DbChange::Insert, id)))
        }
    }

    pub fn insert_breach_email_struct(&self, obj: NewBreachEmail) -> Result<Option<(DbChange, i32)>> {
        let value = &(obj.breach_id, obj.email_id, obj.password.clone());

        if let Some(existing) = BreachEmail::get_opt(self, value)? {
            let id = <BreachEmail as Model>::id(&existing);

            let update = obj.upsert(&existing);
            if update.is_dirty() {
                update.apply(&self)?;
                Ok(Some((DbChange::Update(update.generic()), id)))
            } else {
                Ok(Some((DbChange::None, id)))
            }
        } else {
            diesel::insert_into(breach_emails::table)
                .values(obj)
                .execute(&self.db)?;
            let id = BreachEmail::get_id(self, value)?;
            Ok(Some((DbChange::Insert, id)))
        }
    }

    //

    pub fn update_generic(&self, object: &Update) -> Result<i32> {
        match object {
            Update::Subdomain(object) => self.update_subdomain(object),
            Update::IpAddr(object) => self.update_ipaddr(object),
            Update::Url(object) => self.update_url(object),
            Update::Email(object) => self.update_email(object),
            Update::PhoneNumber(object) => self.update_phonenumber(object),
            Update::Device(object) => self.update_device(object),
            Update::Network(object) => self.update_network(object),
            Update::NetworkDevice(object) => self.update_network_device(object),
            Update::Account(object) => self.update_account(object),
            Update::BreachEmail(object) => self.update_breach_email(object),
        }
    }

    pub fn update_subdomain(&self, subdomain: &SubdomainUpdate) -> Result<i32> {
        use crate::schema::subdomains::columns::*;
        diesel::update(subdomains::table.filter(id.eq(subdomain.id)))
            .set(subdomain)
            .execute(&self.db)?;
        Ok(subdomain.id)
    }

    pub fn update_ipaddr(&self, ipaddr: &IpAddrUpdate) -> Result<i32> {
        use crate::schema::ipaddrs::columns::*;
        diesel::update(ipaddrs::table.filter(id.eq(ipaddr.id)))
            .set(ipaddr)
            .execute(&self.db)?;
        Ok(ipaddr.id)
    }

    pub fn update_url(&self, url: &UrlUpdate) -> Result<i32> {
        use crate::schema::urls::columns::*;
        diesel::update(urls::table.filter(id.eq(url.id)))
            .set(url)
            .execute(&self.db)?;
        Ok(url.id)
    }

    pub fn update_email(&self, email: &EmailUpdate) -> Result<i32> {
        use crate::schema::emails::columns::*;
        diesel::update(emails::table.filter(id.eq(email.id)))
            .set(email)
            .execute(&self.db)?;
        Ok(email.id)
    }

    pub fn update_phonenumber(&self, phonenumber: &PhoneNumberUpdate) -> Result<i32> {
        use crate::schema::phonenumbers::columns::*;
        diesel::update(phonenumbers::table.filter(id.eq(phonenumber.id)))
            .set(phonenumber)
            .execute(&self.db)?;
        Ok(phonenumber.id)
    }

    pub fn update_device(&self, device: &DeviceUpdate) -> Result<i32> {
        use crate::schema::devices::columns::*;
        diesel::update(devices::table.filter(id.eq(device.id)))
            .set(device)
            .execute(&self.db)?;
        Ok(device.id)
    }

    pub fn update_network(&self, network: &NetworkUpdate) -> Result<i32> {
        use crate::schema::networks::columns::*;
        diesel::update(networks::table.filter(id.eq(network.id)))
            .set(network)
            .execute(&self.db)?;
        Ok(network.id)
    }

    pub fn update_network_device(&self, network_device: &NetworkDeviceUpdate) -> Result<i32> {
        use crate::schema::network_devices::columns::*;
        diesel::update(network_devices::table.filter(id.eq(network_device.id)))
            .set(network_device)
            .execute(&self.db)?;
        Ok(network_device.id)
    }

    pub fn update_account(&self, account: &AccountUpdate) -> Result<i32> {
        use crate::schema::accounts::columns::*;
        diesel::update(accounts::table.filter(id.eq(account.id)))
            .set(account)
            .execute(&self.db)?;
        Ok(account.id)
    }

    pub fn update_breach_email(&self, breach_email: &BreachEmailUpdate) -> Result<i32> {
        use crate::schema::breach_emails::columns::*;
        diesel::update(breach_emails::table.filter(id.eq(breach_email.id)))
            .set(breach_email)
            .execute(&self.db)?;
        Ok(breach_email.id)
    }

    fn get_opt_typed<T: Model + Scopable>(&self, value: &T::ID) -> Result<Option<i32>> {
        match T::get_opt(self, &value)? {
            Some(ref obj) if obj.scoped() => Ok(Some(obj.id())),
            _ => Ok(None),
        }
    }

    pub fn get_opt(&self, family: &Family, value: &str) -> Result<Option<i32>> {
        match family {
            Family::Domain => self.get_opt_typed::<Domain>(&value),
            Family::Subdomain => self.get_opt_typed::<Subdomain>(&value),
            Family::IpAddr => self.get_opt_typed::<IpAddr>(&value),
            Family::SubdomainIpAddr => bail!("Unsupported operation"),
            Family::Url => self.get_opt_typed::<Url>(&value),
            Family::Email => self.get_opt_typed::<Email>(&value),
            Family::PhoneNumber => self.get_opt_typed::<PhoneNumber>(&value),
            Family::Device => self.get_opt_typed::<Device>(&value),
            Family::Network => self.get_opt_typed::<Network>(&value),
            Family::NetworkDevice => bail!("Unsupported operation"),
            Family::Account => self.get_opt_typed::<Account>(&value),
            Family::Breach => self.get_opt_typed::<Breach>(&value),
            Family::BreachEmail => bail!("Unsupported operation"),
        }
    }

    //

    pub fn list<T: Model>(&self) -> Result<Vec<T>> {
        T::list(self)
    }

    pub fn filter<T: Model>(&self, filter: &Filter) -> Result<Vec<T>> {
        T::filter(self, filter)
    }

    pub fn filter_with_param<T: Model>(&self, filter: &Filter, param: Option<&String>) -> Result<Vec<T>> {
        match param {
            Some(param) => T::filter_with_param(self, filter, param),
            _ => T::filter(self, filter),
        }
    }

    pub fn scope<T: Scopable>(&self, filter: &Filter) -> Result<usize> {
        T::scope(self, filter)
    }

    pub fn noscope<T: Scopable>(&self, filter: &Filter) -> Result<usize> {
        T::noscope(self, filter)
    }

    pub fn delete<T: Model>(&self, filter: &Filter) -> Result<usize> {
        T::delete(self, filter)
    }
}

#[derive(Debug, PartialEq)]
pub struct Filter {
    query: String,
}

impl Filter {
    pub fn new<I: Into<String>>(query: I) -> Filter {
        Filter {
            query: query.into(),
        }
    }

    fn escape(value: &str) -> String {
        let mut out = String::from("'");
        for c in value.chars() {
            match c {
                '\'' => out.push_str("''"),
                c => out.push(c),
            }
        }
        out.push('\'');
        out
    }

    pub fn parse(mut args: &[String]) -> Result<Filter> {
        debug!("Parsing query: {:?}", args);

        if args.is_empty() {
            bail!("Filter condition is required");
        }

        if args[0].to_lowercase() == "where" {
            args = &args[1..];
        } else {
            bail!("Filter must begin with WHERE");
        }

        let mut query = String::new();

        let mut expect_value = false;

        for arg in args {
            if ["=", "!=", "<", ">", "<=", ">=", "like"].contains(&arg.to_lowercase().as_str()) {
                expect_value = true;
                query += &format!(" {}", arg);
                continue;
            }

            if let Some(idx) = arg.find('=') {
                if idx != 0 {
                    let (key, value) = arg.split_at(idx);
                    query += &format!(" {} = {}", key, Self::escape(&value[1..]));
                    continue;
                }
            }

            if expect_value {
                query.push(' ');
                query.push_str(&Self::escape(arg));
                expect_value = false;
            } else {
                query += &format!(" {}", arg);
            }
        }
        debug!("Parsed query: {:?}", query);

        Ok(Filter::new(query))
    }

    pub fn parse_optional(args: &[String]) -> Result<Filter> {
        debug!("Parsing optional query: {:?}", args);

        if args.is_empty() {
            debug!("Using filter with no condition");
            return Ok(Filter::new("1"));
        }

        Self::parse(args)
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn and_scoped(&self) -> Filter {
        let query = format!("({}) AND unscoped=0", self.query);
        Filter::new(query)
    }

    pub fn sql(&self) -> SqlLiteral<Bool> {
        sql::<Bool>(&self.query)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_simple() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value=1".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value = '1'"));
    }

    #[test]
    fn test_filter_str1() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value=abc".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value = 'abc'"));
    }

    #[test]
    fn test_filter_str2() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value".to_string(),
                                     "=".to_string(),
                                     "asdf".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value = 'asdf'"));
    }

    #[test]
    fn test_filter_and() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value".to_string(),
                                     "=".to_string(),
                                     "foobar".to_string(),
                                     "and".to_string(),
                                     "id".to_string(),
                                     "=".to_string(),
                                     "1".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value = 'foobar' and id = '1'"));
    }

    #[test]
    fn test_filter_like() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value".to_string(),
                                     "like".to_string(),
                                     "%foobar".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value like '%foobar'"));
    }

    #[test]
    fn test_filter_backslash1() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value=\\".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value = '\\'"));
    }

    #[test]
    fn test_filter_backslash2() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value".to_string(),
                                     "=".to_string(),
                                     "\\".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value = '\\'"));
    }

    #[test]
    fn test_filter_quote1() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value=a'b".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value = 'a''b'"));
    }

    #[test]
    fn test_filter_quote2() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value".to_string(),
                                     "=".to_string(),
                                     "a'b".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value = 'a''b'"));
    }

    #[test]
    fn test_filter_greater() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value".to_string(),
                                     ">".to_string(),
                                     "123".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value > '123'"));
    }

    #[test]
    fn test_filter_smaller() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value".to_string(),
                                     "<".to_string(),
                                     "123".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value < '123'"));
    }

    #[test]
    fn test_filter_greater_equal() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value".to_string(),
                                     ">=".to_string(),
                                     "123".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value >= '123'"));
    }

    #[test]
    fn test_filter_smaller_equal() {
        let filter = Filter::parse(&["where".to_string(),
                                     "value".to_string(),
                                     "<=".to_string(),
                                     "123".to_string(),
                                    ]).unwrap();
        assert_eq!(filter, Filter::new(" value <= '123'"));
    }
}
