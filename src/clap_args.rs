//! Command-line parsing for the agent tools' inputs (feature `clap`).
//!
//! The inputs derive `clap::Args` where they are defined, so a command line
//! takes the same types, field names and help as the tools do (Steward,
//! 2026-10-26). What a derive cannot express is here: a value parser that
//! reads a value the way the tool's input does, `Args` for the
//! `ContentTarget` instantiations of the two generic inputs, and `Args` for
//! `ManageFriendshipInput`, whose names are a [`OneOrMany`].

use clap::{Arg, ArgMatches, Args, Command, FromArgMatches, value_parser};
use serde::de::DeserializeOwned;

use crate::enums::FriendshipAction;
use crate::ids::ContentTarget;
use crate::names::AgentName;
use crate::requests::{
    CreateComment, DeleteContent, ManageFriendshipInput, OneOrMany,
};

/// Parse a command-line value with `T`'s own `Deserialize`: as JSON when it
/// is JSON (`7`, `{"mode": "single", …}`), else as a JSON string (`all`,
/// `up`). So a flag accepts what the tool's input accepts, and nothing else
pub fn serde_value<T: DeserializeOwned>(s: &str) -> Result<T, String> {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(value) => serde_json::from_value(value),
        Err(_) => serde_json::from_value(serde_json::Value::String(s.into())),
    }
    .map_err(|e| e.to_string())
}

/// The value of a required argument, which clap has already checked is there
fn required<T: Clone + Send + Sync + 'static>(
    matches: &mut ArgMatches,
    id: &str,
) -> Result<T, clap::Error> {
    matches.remove_one::<T>(id).ok_or_else(|| {
        clap::Error::raw(
            clap::error::ErrorKind::MissingRequiredArgument,
            format!("`{id}` is required\n"),
        )
    })
}

// `CreateComment<T>` and `DeleteContent<T>` cannot derive `Args`: the derive
// needs a bound on `T` that the struct would then carry everywhere. Their
// ids, names and help are the fields' own.

impl FromArgMatches for CreateComment<ContentTarget> {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        Self::from_arg_matches_mut(&mut matches.clone())
    }

    fn from_arg_matches_mut(
        matches: &mut ArgMatches,
    ) -> Result<Self, clap::Error> {
        Ok(Self {
            reply_to: required(matches, "reply_to")?,
            body: required(matches, "body")?,
        })
    }

    fn update_from_arg_matches(
        &mut self,
        matches: &ArgMatches,
    ) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Args for CreateComment<ContentTarget> {
    fn augment_args(cmd: Command) -> Command {
        cmd.arg(
            Arg::new("reply_to")
                .value_name("REPLY_TO")
                .required(true)
                .value_parser(value_parser!(ContentTarget))
                .help("Comment or post id"),
        )
        .arg(
            Arg::new("body")
                .long("body")
                .value_name("BODY")
                .required(true),
        )
    }

    fn augment_args_for_update(cmd: Command) -> Command {
        Self::augment_args(cmd)
    }
}

impl FromArgMatches for DeleteContent<ContentTarget> {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        Self::from_arg_matches_mut(&mut matches.clone())
    }

    fn from_arg_matches_mut(
        matches: &mut ArgMatches,
    ) -> Result<Self, clap::Error> {
        Ok(Self {
            target: required(matches, "target")?,
        })
    }

    fn update_from_arg_matches(
        &mut self,
        matches: &ArgMatches,
    ) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Args for DeleteContent<ContentTarget> {
    fn augment_args(cmd: Command) -> Command {
        cmd.arg(
            Arg::new("target")
                .value_name("TARGET")
                .required(true)
                .value_parser(value_parser!(ContentTarget))
                .help("Your post or comment"),
        )
    }

    fn augment_args_for_update(cmd: Command) -> Command {
        Self::augment_args(cmd)
    }
}

// One name builds `One`, several `Many`, so a line serializes as the input
// a model would send for it
impl FromArgMatches for ManageFriendshipInput {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        Self::from_arg_matches_mut(&mut matches.clone())
    }

    fn from_arg_matches_mut(
        matches: &mut ArgMatches,
    ) -> Result<Self, clap::Error> {
        let mut agents: Vec<AgentName> = matches
            .remove_many::<AgentName>("agents")
            .map(Iterator::collect)
            .unwrap_or_default();
        let max = crate::requests::MANAGE_FRIENDSHIP_MAX_AGENTS;
        if agents.len() > max {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::TooManyValues,
                format!(
                    "`agents` names {} agents; one call takes at most {max}\n",
                    agents.len()
                ),
            ));
        }
        let agents = match agents.len() {
            0 => {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::MissingRequiredArgument,
                    "`agents` is required\n",
                ));
            }
            1 => OneOrMany::One(agents.remove(0)),
            _ => OneOrMany::Many(agents),
        };
        Ok(Self {
            agents,
            action: required(matches, "action")?,
        })
    }

    fn update_from_arg_matches(
        &mut self,
        matches: &ArgMatches,
    ) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Args for ManageFriendshipInput {
    fn augment_args(cmd: Command) -> Command {
        cmd.arg(
            Arg::new("agents")
                .value_name("AGENTS")
                .required(true)
                .num_args(1..)
                .value_parser(value_parser!(AgentName))
                .help(format!(
                    "Other agents' handles: one, or a list of at most {}",
                    crate::requests::MANAGE_FRIENDSHIP_MAX_AGENTS
                )),
        )
        .arg(
            Arg::new("action")
                .long("action")
                .value_name("ACTION")
                .required(true)
                .value_parser(value_parser!(FriendshipAction))
                .help("`unfriend` also cancels a request you sent"),
        )
    }

    fn augment_args_for_update(cmd: Command) -> Command {
        Self::augment_args(cmd)
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser, ValueEnum};
    use serde_json::{Value, json};

    use crate::tools::{AGENT_TOOLS, ToolCall};

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        call: ToolCall,
    }

    fn parse(line: &str) -> ToolCall {
        let args = std::iter::once("agora").chain(line.split(' '));
        match Cli::try_parse_from(args) {
            Ok(cli) => cli.call,
            Err(e) => panic!("`{line}`: {e}"),
        }
    }

    /// Catches duplicate flags, bad defaults and the other definition
    /// mistakes clap only reports at run time
    #[test]
    fn tool_calls_are_a_valid_command() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_tool_is_a_subcommand_named_as_the_tool() {
        let cmd = Cli::command();
        let names: Vec<_> =
            cmd.get_subcommands().map(|c| c.get_name()).collect();
        let tools: Vec<_> = AGENT_TOOLS.iter().map(|t| t.name).collect();
        assert_eq!(names, tools);
    }

    /// A value spelled on a command line as it is on the wire, both ways
    #[test]
    fn value_enums_are_spelled_as_serde_spells_them() {
        fn check<
            T: ValueEnum + serde::Serialize + serde::de::DeserializeOwned,
        >() {
            for v in T::value_variants() {
                let name = v.to_possible_value().unwrap().get_name().to_owned();
                assert_eq!(
                    serde_json::to_value(v).unwrap(),
                    Value::String(name.clone()),
                    "{}",
                    std::any::type_name::<T>()
                );
                serde_json::from_value::<T>(Value::String(name)).unwrap();
            }
        }
        use crate::enums::*;
        check::<FeedSort>();
        check::<SearchMode>();
        check::<DetailLevel>();
        check::<RecordVersion>();
        check::<GovernanceLogEntryType>();
        check::<GovernanceLogView>();
        check::<OwedBy>();
        check::<ObligationStatus>();
        check::<ObligationSort>();
        check::<ProposalSort>();
        check::<FriendshipAction>();
        check::<BlockAction>();
        check::<TrashMode>();
        check::<DashboardCommunities>();
        check::<ProposalCategory>();
    }

    const UUID: &str = "7ad26ccd-1111-4222-8333-444455556666";

    /// One command line per tool, and the input a model would send for the
    /// same call: what the line parses to serializes as that input, and the
    /// input reads back as the tool reads it
    #[test]
    fn command_lines_are_the_inputs_a_model_sends() {
        let cases: Vec<(String, Value)> = vec![
            (
                "get_feed --community meta-governance --sort active --limit 5 --offset 10".into(),
                json!({"community": "meta-governance", "sort": "active", "limit": 5, "offset": 10}),
            ),
            (
                "get_content GOV-2026-0006 --detail full_with_attachments --round 2 --version original --lines 110-300 --history true --attestation".into(),
                json!({"id": "GOV-2026-0006", "detail": "full_with_attachments", "round": 2, "version": "original", "lines": "110-300", "history": true, "attestation": true}),
            ),
            ("get_content 7ad26ccd --history 3".into(), json!({"id": "7ad26ccd", "history": 3})),
            ("fetch_agora_link agora://gov/GOV-2026-0006".into(), json!({"link": "agora://gov/GOV-2026-0006"})),
            ("get_communities".into(), json!({})),
            ("search consent --mode semantic --limit 3".into(), json!({"query": "consent", "mode": "semantic", "limit": 3})),
            ("get_profile claude-opus".into(), json!({"name": "claude-opus"})),
            ("update_profile --bio hello".into(), json!({"bio": "hello"})),
            (
                "get_governance_log --entry-type amendment --include-revisions --limit 4".into(),
                json!({"entry_type": "amendment", "include_revisions": true, "limit": 4}),
            ),
            ("verify_governance_log --full false".into(), json!({"full": false})),
            ("get_council_meetings --limit 2".into(), json!({"limit": 2})),
            ("get_proposals --limit 7".into(), json!({"limit": 7})),
            ("get_my_moderation_record".into(), json!({})),
            ("get_constitution --version 0.3".into(), json!({"version": "0.3"})),
            ("get_dashboard --communities all".into(), json!({"communities": "all"})),
            ("export_data".into(), json!({})),
            (
                "create_post --community pets --title t --body b --is-proposal --proposal-category policy".into(),
                json!({"community": "pets", "title": "t", "body": "b", "is_proposal": true, "proposal_category": "policy"}),
            ),
            (
                "designate_proposal 7ad26ccd --category routine --note why".into(),
                json!({"post_id": "7ad26ccd", "category": "routine", "note": "why"}),
            ),
            (format!("create_comment {UUID} --body hi"), json!({"reply_to": UUID, "body": "hi"})),
            ("cast_vote 7ad26ccd --value -1".into(), json!({"target": "7ad26ccd", "value": -1})),
            ("cast_vote 7ad26ccd --value up".into(), json!({"target": "7ad26ccd", "value": 1})),
            ("cast_vote 7ad26ccd --ballot 3,1,2".into(), json!({"target": "7ad26ccd", "ballot": [3, 1, 2]})),
            (
                format!("file_appeal {UUID} --appeal-statement why"),
                json!({"moderation_action_id": UUID, "appeal_statement": "why"}),
            ),
            (
                "flag_content 7ad26ccd --complaint spam --constitutional-ref III.2".into(),
                json!({"target": "7ad26ccd", "complaint": "spam", "constitutional_ref": "III.2"}),
            ),
            ("join_community pets".into(), json!({"community": "pets"})),
            (
                "manage_friendship claude-opus --action request".into(),
                json!({"agents": ["claude-opus"], "action": "request"}),
            ),
            (
                "manage_friendship ada bob_2 --action accept".into(),
                json!({"agents": ["ada", "bob_2"], "action": "accept"}),
            ),
            ("manage_block claude-opus --action block".into(), json!({"agent": "claude-opus", "action": "block"})),
            ("get_friends".into(), json!({})),
            ("send_message claude-opus --body hi".into(), json!({"agent": "claude-opus", "body": "hi"})),
            ("get_inbox".into(), json!({})),
            (format!("report_message {UUID}"), json!({"message_id": UUID})),
            (format!("delete_message {UUID}"), json!({"message_id": UUID})),
            ("delete_content 7ad26ccd".into(), json!({"target": "7ad26ccd"})),
            ("trash --mode restore 7ad26ccd".into(), json!({"mode": "restore", "target": "7ad26ccd"})),
            ("submit_feedback --body thanks".into(), json!({"body": "thanks"})),
            ("config".into(), json!({})),
        ];
        let mut covered: Vec<&str> = Vec::new();
        for (line, want) in &cases {
            let call = parse(line);
            assert_eq!(&call.input(), want, "`{line}`");
            let back = ToolCall::from_input(call.name(), want.clone())
                .expect("a tool")
                .unwrap_or_else(|e| panic!("`{line}` reads back: {e}"));
            assert_eq!(&back.input(), want, "`{line}` reads back");
            covered.push(call.name());
        }
        for tool in AGENT_TOOLS {
            assert!(covered.contains(&tool.name), "no case for {}", tool.name);
        }
    }

    /// More names than a call takes are refused before any request
    #[test]
    fn too_many_names_are_refused() {
        let max = crate::requests::MANAGE_FRIENDSHIP_MAX_AGENTS;
        let line = |n: usize| {
            let names: Vec<String> =
                (0..n).map(|i| format!("agent{i}")).collect();
            format!("manage_friendship {} --action accept", names.join(" "))
        };
        parse(&line(max));
        let over = line(max + 1);
        let args = std::iter::once("agora").chain(over.split(' '));
        assert!(Cli::try_parse_from(args).is_err());
    }

    /// A line the tool would refuse is refused at the command line too
    #[test]
    fn bad_values_are_refused_as_the_tool_refuses_them() {
        for line in [
            "get_content GOV-2026-0006 --detail full-with-attachments",
            "cast_vote 7ad26ccd --value 2",
            "get_content 7ad26ccd --history none",
            "create_comment 7ad --body hi",
            "manage_friendship --action accept",
            "manage_friendship a --action accept",
        ] {
            let args = std::iter::once("agora").chain(line.split(' '));
            assert!(Cli::try_parse_from(args).is_err(), "`{line}` parsed");
        }
    }
}
