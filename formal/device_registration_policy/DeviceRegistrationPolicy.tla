---- MODULE DeviceRegistrationPolicy ----
EXTENDS FiniteSets

CONSTANTS
    Scenarios,
    ExistingOwnerScenarios,
    BugAllowAdditionalDeviceBeforeRelayConfirmation,
    BugRequireRelayConfirmationForBootstrap

ASSUME Scenarios # {}
ASSUME ExistingOwnerScenarios \subseteq Scenarios
ASSUME BugAllowAdditionalDeviceBeforeRelayConfirmation \in BOOLEAN
ASSUME BugRequireRelayConfirmationForBootstrap \in BOOLEAN

VARIABLES
    localPublished,
    relayVisible,
    inviteAccepted,
    relayUp

vars ==
    <<localPublished, relayVisible, inviteAccepted, relayUp>>

HasPreviousDevices(s) ==
    s \in ExistingOwnerScenarios

RouteReady(s) ==
    IF HasPreviousDevices(s)
        THEN IF BugAllowAdditionalDeviceBeforeRelayConfirmation
            THEN localPublished[s] \/ relayVisible[s]
            ELSE relayVisible[s]
        ELSE IF BugRequireRelayConfirmationForBootstrap
            THEN relayVisible[s]
            ELSE localPublished[s] \/ relayVisible[s]

Init ==
    /\ localPublished = [s \in Scenarios |-> FALSE]
    /\ relayVisible = [s \in Scenarios |-> FALSE]
    /\ inviteAccepted = [s \in Scenarios |-> FALSE]
    /\ relayUp = TRUE

PublishLocal(s) ==
    /\ ~localPublished[s]
    /\ localPublished' = [localPublished EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<relayVisible, inviteAccepted, relayUp>>

RelaySync(s) ==
    /\ relayUp
    /\ localPublished[s]
    /\ ~relayVisible[s]
    /\ relayVisible' = [relayVisible EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<localPublished, inviteAccepted, relayUp>>

AcceptViaPublicInvite(s) ==
    /\ ~inviteAccepted[s]
    /\ RouteReady(s)
    /\ inviteAccepted' = [inviteAccepted EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<localPublished, relayVisible, relayUp>>

RelayPartition ==
    /\ relayUp
    /\ relayUp' = FALSE
    /\ UNCHANGED <<localPublished, relayVisible, inviteAccepted>>

RelayRecover ==
    /\ ~relayUp
    /\ relayUp' = TRUE
    /\ UNCHANGED <<localPublished, relayVisible, inviteAccepted>>

\* Explicit delay step while waiting for relay visibility.
RelayDelay ==
    /\ \E s \in Scenarios: localPublished[s] /\ ~relayVisible[s]
    /\ UNCHANGED vars

Stutter ==
    UNCHANGED vars

RelayEventuallyRecovers ==
    <>[]relayUp

Next ==
    \/ \E s \in Scenarios: PublishLocal(s)
    \/ \E s \in Scenarios: RelaySync(s)
    \/ \E s \in Scenarios: AcceptViaPublicInvite(s)
    \/ RelayPartition
    \/ RelayRecover
    \/ RelayDelay
    \/ Stutter

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ \A s \in Scenarios: WF_vars(PublishLocal(s))
    /\ \A s \in Scenarios: WF_vars(RelaySync(s))
    /\ \A s \in Scenarios: WF_vars(AcceptViaPublicInvite(s))

SpecUnderRecovery ==
    /\ Spec
    /\ RelayEventuallyRecovers

AdditionalDeviceNeedsRelayConfirmation ==
    \A s \in ExistingOwnerScenarios:
        localPublished[s] /\ ~relayVisible[s] => ~RouteReady(s)

BootstrapAllowsLocalConfirmation ==
    \A s \in Scenarios \ ExistingOwnerScenarios:
        localPublished[s] /\ ~relayVisible[s] => RouteReady(s)

AllScenariosEventuallyAcceptUnderRecovery ==
    \A s \in Scenarios:
        <>inviteAccepted[s]

====
