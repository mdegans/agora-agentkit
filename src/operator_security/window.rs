//! The check window (agora `docs/design-operator-posting.md`, *One rule for
//! the web and the CLI*): a passkey check opens it, tied to the passkey and
//! the client's network, and it lasts by the operator's [`Tier`]. The server
//! holds its state; nothing here performs I/O, and time is an argument

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use chrono::{DateTime, Duration, Utc};

use super::challenge::sha256;
use crate::enums::{CheckSurface, CheckWindowEnd};

/// An operator's size, by `tier_agent_count` (live agents and those in their
/// departure grace), read at each request and never cached. The one home of
/// the 5, 50 and 500 lines
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    UpTo5,
    UpTo50,
    UpTo500,
    Over500,
}

impl Tier {
    pub const ALL: [Self; 4] =
        [Self::UpTo5, Self::UpTo50, Self::UpTo500, Self::Over500];

    pub const fn of(count: u32) -> Self {
        match count {
            0..=5 => Self::UpTo5,
            6..=50 => Self::UpTo50,
            51..=500 => Self::UpTo500,
            _ => Self::Over500,
        }
    }

    /// [`Self::of`] for a database count. A negative one is drift, and reads
    /// as the strictest tier
    pub const fn of_i64(count: i64) -> Self {
        if count < 0 || count > u32::MAX as i64 {
            Self::Over500
        } else {
            Self::of(count as u32)
        }
    }

    /// The most agents this tier holds: 5, 50, 500, and `u32::MAX` over 500.
    /// `Tier::UpTo5.max_agents()` is the most an operator signing with a
    /// software key may have
    pub const fn max_agents(self) -> u32 {
        match self {
            Self::UpTo5 => 5,
            Self::UpTo50 => 50,
            Self::UpTo500 => 500,
            Self::Over500 => u32::MAX,
        }
    }

    /// How long a check window lasts
    pub fn window(self) -> WindowLimits {
        let (idle, absolute) = match self {
            Self::UpTo5 => (Duration::hours(12), Duration::days(2)),
            Self::UpTo50 => (Duration::hours(1), Duration::days(1)),
            Self::UpTo500 => (Duration::minutes(15), Duration::days(1)),
            Self::Over500 => (Duration::minutes(5), Duration::days(1)),
        };
        WindowLimits { idle, absolute }
    }
}

/// A [`Tier`]'s check window
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowLimits {
    /// Ends it after this long unused
    pub idle: Duration,
    /// Ends it this long after it opened, however it is used
    pub absolute: Duration,
}

/// The client's network, as a check window compares it: an IPv4 address,
/// or an IPv6 address's /64 (privacy addresses rotate within it). An
/// IPv4-mapped IPv6 address is read as IPv4. It names a network, not a
/// device
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientNet {
    V4(Ipv4Addr),
    /// The first 64 bits
    V6([u8; 8]),
}

impl ClientNet {
    /// IPv4's tag in [`Self::canonical_bytes`]
    pub const V4_TAG: u8 = 4;
    /// IPv6's tag in [`Self::canonical_bytes`]
    pub const V6_TAG: u8 = 6;

    pub fn of(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => Self::V4(v4),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self::V4(v4),
                None => {
                    let mut prefix = [0; 8];
                    prefix.copy_from_slice(&v6.octets()[..8]);
                    Self::V6(prefix)
                }
            },
        }
    }

    /// Whether `ip` is on this network
    pub fn contains(&self, ip: IpAddr) -> bool {
        Self::of(ip) == *self
    }

    /// `4 ‖ the address (4)` or `6 ‖ the /64 prefix (8)`
    pub fn canonical_bytes(&self) -> Vec<u8> {
        match self {
            Self::V4(v4) => {
                let mut out = vec![Self::V4_TAG];
                out.extend(v4.octets());
                out
            }
            Self::V6(prefix) => {
                let mut out = vec![Self::V6_TAG];
                out.extend(prefix);
                out
            }
        }
    }

    /// SHA-256 of [`Self::canonical_bytes`], as tap 0x15 binds it
    pub fn sha256(&self) -> [u8; 32] {
        sha256(&self.canonical_bytes())
    }

    /// The network's address and prefix length, for a `CIDR` column: `/32`
    /// for IPv4, `/64` for IPv6, host bits zero
    pub fn cidr(&self) -> (IpAddr, u8) {
        match self {
            Self::V4(v4) => (IpAddr::V4(*v4), 32),
            Self::V6(prefix) => {
                let mut octets = [0; 16];
                octets[..8].copy_from_slice(prefix);
                (IpAddr::V6(Ipv6Addr::from(octets)), 64)
            }
        }
    }

    /// A stored network read back: refused unless it is exactly what
    /// [`Self::cidr`] writes
    pub fn from_cidr(ip: IpAddr, prefix: u8) -> Result<Self, NotAClientNet> {
        let net = Self::of(ip);
        match (ip, prefix) {
            (IpAddr::V4(_), 32) => Ok(net),
            (IpAddr::V6(v6), 64)
                if v6.to_ipv4_mapped().is_none()
                    && v6.octets()[8..] == [0; 8] =>
            {
                Ok(net)
            }
            _ => Err(NotAClientNet { ip, prefix }),
        }
    }
}

impl From<IpAddr> for ClientNet {
    fn from(ip: IpAddr) -> Self {
        Self::of(ip)
    }
}

/// `203.0.113.7/32`, `2001:db8:1:2::/64`
impl std::fmt::Display for ClientNet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (ip, prefix) = self.cidr();
        write!(f, "{ip}/{prefix}")
    }
}

/// A `CIDR` value that isn't a [`ClientNet`]: IPv4 other than /32, IPv6 other
/// than /64, host bits set, or an IPv4-mapped IPv6 network
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{ip}/{prefix} is not a client network (IPv4 /32 or IPv6 /64)")]
pub struct NotAClientNet {
    pub ip: IpAddr,
    pub prefix: u8,
}

/// An open check window's state (`operator_check_windows`): its idle and
/// absolute limits come from the [`Tier`] at each read, so a count that
/// crosses a line shortens it at once
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckWindow {
    pub surface: CheckSurface,
    pub net: ClientNet,
    pub opened_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
}

impl CheckWindow {
    /// When it ends if unused from now on, absolute limit aside
    pub fn idle_expires_at(&self, tier: Tier) -> DateTime<Utc> {
        self.last_used_at + tier.window().idle
    }

    /// When it ends however it is used
    pub fn expires_at(&self, tier: Tier) -> DateTime<Utc> {
        self.opened_at + tier.window().absolute
    }

    /// Why it has lapsed by `now`, if it has: whichever limit came first, and
    /// at a tie the absolute one. A window is over at its limit, not after
    pub fn lapsed(
        &self,
        tier: Tier,
        now: DateTime<Utc>,
    ) -> Option<CheckWindowEnd> {
        let idle = self.idle_expires_at(tier);
        let absolute = self.expires_at(tier);
        let (first, reason) = if absolute <= idle {
            (absolute, CheckWindowEnd::Absolute)
        } else {
            (idle, CheckWindowEnd::Idle)
        };
        (now >= first).then_some(reason)
    }

    /// Whether a request from `ip` at `now` may use it: refused with why it
    /// ends otherwise. A lapse is reported before a change of network, since
    /// it happened first
    pub fn check(
        &self,
        tier: Tier,
        now: DateTime<Utc>,
        ip: IpAddr,
    ) -> Result<(), CheckWindowEnd> {
        if let Some(reason) = self.lapsed(tier, now) {
            return Err(reason);
        }
        if !self.net.contains(ip) {
            return Err(CheckWindowEnd::Address);
        }
        Ok(())
    }

    /// What the portal's bootstrap says of it
    pub fn status(&self, tier: Tier) -> crate::operator::CheckWindowStatus {
        crate::operator::CheckWindowStatus {
            surface: self.surface,
            opened_at: self.opened_at,
            idle_expires_at: self.idle_expires_at(tier),
            expires_at: self.expires_at(tier),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// The table at each edge: 5/6, 50/51, 500/501
    #[test]
    fn tiers_at_every_line() {
        let cases = [
            (0, Tier::UpTo5),
            (5, Tier::UpTo5),
            (6, Tier::UpTo50),
            (50, Tier::UpTo50),
            (51, Tier::UpTo500),
            (500, Tier::UpTo500),
            (501, Tier::Over500),
            (u32::MAX, Tier::Over500),
        ];
        for (count, tier) in cases {
            assert_eq!(Tier::of(count), tier, "{count}");
            assert_eq!(Tier::of_i64(i64::from(count)), tier, "{count}");
        }
        // Drift reads as the strictest
        assert_eq!(Tier::of_i64(-1), Tier::Over500);
        assert_eq!(Tier::of_i64(i64::from(u32::MAX) + 1), Tier::Over500);
        // Each line is its tier's most, and the next count is the next tier
        for tier in &Tier::ALL[..3] {
            assert_eq!(Tier::of(tier.max_agents()), *tier);
            assert!(Tier::of(tier.max_agents() + 1) > *tier);
        }
        assert_eq!(Tier::ALL.map(Tier::max_agents), [5, 50, 500, u32::MAX]);
    }

    #[test]
    fn windows_by_tier() {
        let h = Duration::hours;
        let m = Duration::minutes;
        let d = Duration::days;
        let want = [
            (Tier::UpTo5, h(12), d(2)),
            (Tier::UpTo50, h(1), d(1)),
            (Tier::UpTo500, m(15), d(1)),
            (Tier::Over500, m(5), d(1)),
        ];
        for (tier, idle, absolute) in want {
            assert_eq!(tier.window(), WindowLimits { idle, absolute });
        }
        // At each line, one more agent shortens the idle window
        for (low, high) in [(5, 6), (50, 51), (500, 501)] {
            assert!(Tier::of(high).window().idle < Tier::of(low).window().idle);
        }
    }

    #[test]
    fn networks_normalise() {
        // IPv4: the address
        assert_eq!(
            ClientNet::of(ip("203.0.113.7")),
            ClientNet::V4("203.0.113.7".parse().unwrap())
        );
        assert_ne!(
            ClientNet::of(ip("203.0.113.7")),
            ClientNet::of(ip("203.0.113.8"))
        );
        // IPv6: the /64
        let a = ClientNet::of(ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd"));
        assert_eq!(a, ClientNet::of(ip("2001:db8:1:2::1")));
        assert_eq!(a, ClientNet::V6([0x20, 0x01, 0x0d, 0xb8, 0, 1, 0, 2]));
        assert_ne!(a, ClientNet::of(ip("2001:db8:1:3::1")));
        // IPv4-mapped IPv6 is IPv4
        assert_eq!(
            ClientNet::of(ip("::ffff:203.0.113.7")),
            ClientNet::of(ip("203.0.113.7"))
        );
        // The canonical bytes and their digest (Python `hashlib`)
        let v4 = ClientNet::of(ip("203.0.113.7"));
        assert_eq!(v4.canonical_bytes(), [4, 203, 0, 113, 7]);
        assert_eq!(
            hex::encode(v4.sha256()),
            "3a006b152c11065cfe9608a4dab25f23b71ceaf55506cafb31cfe61f13ae1c5a"
        );
        assert_eq!(
            a.canonical_bytes(),
            [6, 0x20, 0x01, 0x0d, 0xb8, 0, 1, 0, 2]
        );
        assert_eq!(
            hex::encode(a.sha256()),
            "a0fbe0097ab82f496687c337383f95ecacc36f13bddf0f00c26b5d4419975ac4"
        );
        // Display and the stored CIDR, both ways
        assert_eq!(v4.to_string(), "203.0.113.7/32");
        assert_eq!(a.to_string(), "2001:db8:1:2::/64");
        for net in [v4, a] {
            let (addr, prefix) = net.cidr();
            assert_eq!(ClientNet::from_cidr(addr, prefix), Ok(net));
        }
        for (addr, prefix) in [
            ("203.0.113.0", 24),
            ("2001:db8:1:2::", 48),
            ("2001:db8:1:2::1", 64),
            ("::ffff:203.0.113.7", 64),
            ("2001:db8:1:2::", 128),
        ] {
            assert!(ClientNet::from_cidr(ip(addr), prefix).is_err(), "{addr}");
        }
    }

    fn window(net: &str) -> CheckWindow {
        let t0 = DateTime::from_timestamp(1_761_782_400, 0).unwrap();
        CheckWindow {
            surface: CheckSurface::Web,
            net: ClientNet::of(ip(net)),
            opened_at: t0,
            last_used_at: t0,
        }
    }

    #[test]
    fn a_window_lapses_at_its_limits() {
        let w = window("203.0.113.7");
        let here = ip("203.0.113.7");
        for tier in Tier::ALL {
            let limits = tier.window();
            let idle_at = w.opened_at + limits.idle;
            let second = Duration::seconds(1);
            assert_eq!(w.check(tier, idle_at - second, here), Ok(()));
            assert_eq!(
                w.check(tier, idle_at, here),
                Err(CheckWindowEnd::Idle),
                "{tier:?}"
            );
            // Kept in use, it still ends at the absolute limit
            let abs_at = w.opened_at + limits.absolute;
            let busy = CheckWindow {
                last_used_at: abs_at - second,
                ..w
            };
            assert_eq!(busy.check(tier, abs_at - second, here), Ok(()));
            assert_eq!(
                busy.check(tier, abs_at, here),
                Err(CheckWindowEnd::Absolute)
            );
            let status = busy.status(tier);
            assert_eq!(status.expires_at, abs_at);
            assert_eq!(status.idle_expires_at, abs_at - second + limits.idle);
            assert_eq!(status.ends_at(), abs_at);
        }
    }

    /// Read at each request: a count that crosses a line shortens an open
    /// window at once
    #[test]
    fn a_tier_crossing_shortens_an_open_window() {
        let w = window("203.0.113.7");
        let now = w.opened_at + Duration::hours(2);
        let here = ip("203.0.113.7");
        assert_eq!(w.check(Tier::of(5), now, here), Ok(()));
        assert_eq!(w.check(Tier::of(6), now, here), Err(CheckWindowEnd::Idle));
    }

    /// Any change of network ends it, on every tier: an IPv4 change, and an
    /// IPv6 address leaving its /64; one moving within it keeps it
    #[test]
    fn a_change_of_network_ends_it() {
        let v4 = window("203.0.113.7");
        let v6 = window("2001:db8:1:2::1");
        for tier in Tier::ALL {
            let now = v4.opened_at;
            assert_eq!(
                v4.check(tier, now, ip("203.0.113.8")),
                Err(CheckWindowEnd::Address)
            );
            assert_eq!(v4.check(tier, now, ip("::ffff:203.0.113.7")), Ok(()));
            assert_eq!(
                v6.check(tier, now, ip("2001:db8:1:2:dead:beef:0:9")),
                Ok(())
            );
            assert_eq!(
                v6.check(tier, now, ip("2001:db8:1:3::1")),
                Err(CheckWindowEnd::Address)
            );
            assert_eq!(
                v6.check(tier, now, ip("203.0.113.7")),
                Err(CheckWindowEnd::Address)
            );
            // A lapse is what ended it, when it came first
            let late = now + tier.window().absolute;
            assert_eq!(
                v4.check(tier, late, ip("203.0.113.8")),
                Err(CheckWindowEnd::Idle)
            );
        }
    }
}
