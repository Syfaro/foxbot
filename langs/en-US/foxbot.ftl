-botName = @FoxBot
-creatorName = @Syfaro
-newsChannelName = @FoxBotNews
-docsLink = https://syfaro.net/blog/foxbot/

# General Onboarding
welcome =
    Hi, I'm { -botName }.
    
    If you use me as an inline bot, I'll mirror content from many furry sites. When I post the image I'll include a direct link and a source link, if available. You can add your Twitter account with /twitter to get content from locked accounts you follow.
    
    If you send me an image, I'll try to find the source on FA.
    
    Add me to your group for features like /mirror (where I mirror all the links in a message, including messages you reply to) or /source (where I attempt to find the source of an image you're replying to).
    
    If I'm given edit permissions in your channel, I'll automatically edit posts to include a source link.
    
    For more details, visit { -docsLink }. Also check out { -newsChannelName } for feature updates.
    
    Contact my creator { -creatorName } if you have any issues or feature suggestions.

welcome-group =
    Hi, I'm { -botName }.
    
    I'm here to help with sharing images! If you use me as an inline bot, I can easily get images from most furry sites, while keeping a link to the original source.
    
    I've also got a few commands to help in groups:
    · /mirror - I'll look at all the links in your message or the message you're replying to and mirror them
    · /source - I'll attempt to find if the photo you're replying to has been posted on FurAffinity
    
    You can also enable automatically finding sources for images posted in here with the /groupsource command. However, I must be an administrator in the group for this to work and it can only be enabled by an administrator.
    
    For more details, visit { -docsLink }. Also check out { -newsChannelName } for feature updates.

welcome-try-me = Try Me!

# Inline Keyboard
inline-direct = Direct Link
inline-source = Source

# No Inline Results
inline-no-results-title = No results found
inline-no-results-body = I could not find any results for the provided query.

# Inline Results Misc
inline-help = Help
inline-process = Process Video

# Inline Videos
video-starting = Starting to process video...
video-progress = Video processing is { $percent } complete...
video-finished = Finished transcoding, uploading video...
video-return-button = Return and send
video-unknown = Sorry, something went wrong.

# Reverse Search
reverse-no-results = I was unable to find anything, sorry.
reverse-result = I found this: { $link } ({ $rating })
reverse-result-unknown = I found this: { $link }
reverse-multiple-results = I found multiple sources:
reverse-multiple-item = · { $link } ({ $rating })
reverse-multiple-item-unknown = · { $link }
reverse-subscribe = Notify Me

# Twitter Onboarding
twitter-callback = Please follow this link to add your Twitter account: { $link }
twitter-welcome = Welcome aboard, { $userName }!
twitter-private = Let's do this in a private chat.
twitter-not-for-you = It doesn't look like anything to me
twitter-existing-account = It looks like you already have the account { $account } added. Are you sure you want to change this?
twitter-change-anyway = Change Account 
twitter-remove-account = Remove Account
twitter-removed-account = Okay, got it. Your Twitter account has been removed.

# In-group mirroring
mirror-no-links = Sorry, I could not find any links to mirror images from.
mirror-no-results = Sorry, I could not get any images from this message.
mirror-missing =
    I could not fetch images from the following links:
    
    { $links }
    
    Sorry about that.

# In-group sourcing
source-no-photo = Sorry, I can't seem to find the photo here.

# In-group alternates
alternate-title = Here are some possible alternate versions:
alternate-posted-by = Posted by { $name }
alternate-distance = · { $link } ({ $rating }, distance of { $distance })
alternate-distance-unknown = · { $link } (distance of { $distance })
alternate-multiple-photo = I can only find alternates for a single photo, sorry.
alternate-unknown-link = Sorry, I don't understand the provided link.

# Automatic group sourcing
automatic-single = It looks like this image may have come from here: { $link } ({ $rating })
automatic-single-unknown = It looks like this image may have come from here: { $link }
automatic-multiple = I found a few places this image may have come from:
automatic-multiple-result = · { $link } ({ $rating })
automatic-multiple-result-unknown = · { $link }
automatic-enable-not-admin = Sorry, you must be a group admin to enable this.
automatic-enable-bot-not-admin = Sorry, you must give me admin permissions due to a Telegram limitation.
automatic-enable-success = Automatic group sourcing is now enabled!
automatic-disable = This feature is now turned off.
automatic-enable-not-group = This feature is only supported in groups.
automatic-preview-disable = Sourced image previews disabled.
automatic-preview-enable = Sourced image previews enabled.
automatic-album-enable = In-group album sourcing enabled.
automatic-album-disable = In-group album sourcing disabled.

# Error Messages
error-generic = Oh no, something went wrong! Please send a message to my creator, { -creatorName }, saying what happened.
error-generic-message =
    Oh no, something went wrong: { $message }
    
    Please send a message to my creator, { -creatorName }, saying what happened.
error-generic-count = Oh no, something went wrong! I've encountered { $count } errors. Please send a message to my creator, { -creatorName }, saying what happened.
error-uuid = Oh no, something went wrong! Please reply to this message saying what happened. You may also send a message to my creator, { -creatorName }, with this ID if you continue having issues: { $uuid }
error-uuid-message =
    Oh no, something went wrong: { $message }
    
    Please reply to this message saying what happened. You may also send a message to my creator, { -creatorName }, with this ID if you continue having issues: { $uuid }
error-uuid-count = Oh no, something went wrong! I've encountered { $count } errors. Please reply to this message saying what happened. You may also send a message to my creator, { -creatorName }, with this ID if you continue having issues: { $uuid }
error-feedback = Thank you for the feedback, hopefully we can get this issue resolved soon.
error-delete-callback = Error retrieving message to delete 
error-deleted = Deleted message

# Settings
settings-main = Let's take a look at some settings.
settings-site-order =
    Set the order of sites in returned results.
    
    This is especially useful for places where only one result is returned.
settings-unsupported = Sorry, not yet supported.
settings-move-unable = Unable to move { $name } to that position
settings-move-updated = Updated position for { $name }
settings-site-preference = Site Preference

rating-general = SFW
rating-adult = NSFW
rating-unknown = Unknown

subscribe-error = Unable to subscribe to notifications
subscribe-success = I'll let you know if I find this later!
subscribe-found-single =
    I found a match to an image you were looking for!
    
    You can see it here: { $link }
subscribe-found-multiple = I found matches for an image you were looking for!
subscribe-found-multiple-item = · { $link }
